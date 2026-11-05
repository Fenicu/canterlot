export interface DraftRecord {
  text: string;
  revision: number;
  updated_at?: string | null;
  device_id?: string | null;
}

export interface DraftSyncLocal {
  load(): Promise<{
    text: string;
    updatedAt: number;
    attachments?: { id: string; name?: string; size?: number }[];
  } | null>;
  set(text: string): void;
  clear(): void;
}

export interface DraftSyncApi {
  put(text: string, baseRevision: number): Promise<DraftRecord>;
}

export interface DraftSyncOptions {
  deviceId: string;
  api: DraftSyncApi;
  local: DraftSyncLocal;
  current(): string;
  apply(text: string): void;
  now(): number;
  schedule(fn: () => void, ms: number): () => void;
  online(): boolean;
  debounceMs?: number;
  idleMs?: number;
}

/**
 * Общий черновик поля ввода. Сервер хранит текст и ревизию; локальная копия
 * (DraftStore) остаётся буфером на случай офлайна. Эхо собственной записи
 * отбрасывается по `device_id`, чужое применяется сразу, если владелец не печатает,
 * иначе — после паузы в `idleMs`.
 */
export class DraftSync {
  readonly #options: DraftSyncOptions;
  readonly #debounceMs: number;
  readonly #idleMs: number;
  #revision = 0;
  #savedText = "";
  #dirty = false;
  #started = false;
  #lastEditAt = 0;
  #saveTimer: (() => void) | null = null;
  #idleTimer: (() => void) | null = null;
  #pending: DraftRecord | null = null;
  #saving = false;
  #queued = false;

  constructor(options: DraftSyncOptions) {
    this.#options = options;
    this.#debounceMs = options.debounceMs ?? 500;
    this.#idleMs = options.idleMs ?? 2_000;
  }

  get revision(): number {
    return this.#revision;
  }

  get dirty(): boolean {
    return this.#dirty;
  }

  /** Загружает локальный буфер и выбирает более новую копию: серверную или локальную. */
  async start(server: DraftRecord | null): Promise<{
    id: string;
    name?: string;
    size?: number;
  }[]> {
    if (this.#started) return [];
    this.#started = true;
    this.#revision = server?.revision ?? 0;
    this.#savedText = server?.text ?? "";
    const local = await this.#options.local.load();
    const attachments = local?.attachments ?? [];
    const serverText = server?.text ?? "";
    const serverTime = server?.updated_at ? Date.parse(server.updated_at) : 0;
    const localTime = local?.updatedAt ?? 0;
    const localText = local?.text ?? "";

    if (local && localText !== serverText && localTime > serverTime) {
      this.#applyLocal(localText);
    } else if (server) {
      this.#options.apply(serverText);
      this.#savedText = serverText;
      this.#dirty = false;
      this.#options.local.set(serverText);
    } else if (localText) {
      this.#applyLocal(localText);
    }
    return attachments;
  }

  /** Каждое нажатие: локальный буфер сразу, сервер через `debounceMs` после последнего. */
  typed(): void {
    this.#lastEditAt = this.#options.now();
    const text = this.#options.current();
    this.#dirty = text !== this.#savedText;
    this.#options.local.set(text);
    if (this.#dirty) this.#scheduleSave();
    if (this.#pending) this.#scheduleIdle();
  }

  /** Чужое событие: эхо своего устройства и старые ревизии отбрасываются. */
  remote(record: DraftRecord | null): void {
    if (!record) return;
    if (record.device_id && record.device_id === this.#options.deviceId) return;
    if (record.revision <= this.#revision) return;
    this.#revision = record.revision;
    this.#savedText = record.text;

    if (this.#options.current() === record.text) {
      this.#dirty = false;
      this.#pending = null;
      this.#cancelIdle();
      this.#options.local.set(record.text);
      return;
    }
    if (!this.#dirty || this.#options.now() - this.#lastEditAt >= this.#idleMs) {
      this.#applyRemote(record);
      return;
    }
    this.#pending = record;
    this.#scheduleIdle();
  }

  /** Немедленная запись: потеря фокуса, скрытие вкладки, уход с экрана, возврат сети. */
  async flush(): Promise<void> {
    this.#cancelSave();
    if (!this.#dirty || !this.#options.online()) return;
    if (this.#saving) {
      this.#queued = true;
      return;
    }
    this.#saving = true;
    const text = this.#options.current();
    try {
      const record = await this.#options.api.put(text, this.#revision);
      this.#revision = Math.max(this.#revision, record.revision);
      this.#savedText = record.text;
      this.#dirty = this.#options.current() !== record.text;
      if (this.#dirty) this.#scheduleSave();
      else this.#options.local.set(record.text);
    } catch {
      // Локальный буфер уже сохранён; повтор будет при следующем нажатии, flush или возврате сети.
    } finally {
      this.#saving = false;
      if (this.#queued) {
        this.#queued = false;
        void this.flush();
      }
    }
  }

  async leave(): Promise<void> {
    this.#cancelIdle();
    this.#pending = null;
    await this.flush();
  }

  /** Ввод ушёл в очередь: локальная копия и отложенная запись больше не нужны. */
  sent(): void {
    this.#cancelSave();
    this.#cancelIdle();
    this.#pending = null;
    this.#dirty = false;
    this.#savedText = "";
    this.#options.local.clear();
  }

  #applyLocal(text: string): void {
    this.#options.apply(text);
    this.#options.local.set(text);
    this.#dirty = text !== this.#savedText;
    if (this.#dirty) this.#scheduleSave();
  }

  #applyRemote(record: DraftRecord): void {
    this.#cancelSave();
    this.#cancelIdle();
    this.#pending = null;
    this.#dirty = false;
    this.#savedText = record.text;
    this.#revision = Math.max(this.#revision, record.revision);
    this.#options.apply(record.text);
    this.#options.local.set(record.text);
  }

  #scheduleSave(): void {
    this.#cancelSave();
    this.#saveTimer = this.#options.schedule(() => {
      this.#saveTimer = null;
      void this.flush();
    }, this.#debounceMs);
  }

  #scheduleIdle(): void {
    this.#cancelIdle();
    if (!this.#pending) return;
    const wait = Math.max(
      0,
      this.#idleMs - (this.#options.now() - this.#lastEditAt),
    );
    this.#idleTimer = this.#options.schedule(() => {
      this.#idleTimer = null;
      const record = this.#pending;
      if (record) this.#applyRemote(record);
    }, wait);
  }

  #cancelSave(): void {
    this.#saveTimer?.();
    this.#saveTimer = null;
  }

  #cancelIdle(): void {
    this.#idleTimer?.();
    this.#idleTimer = null;
  }
}
