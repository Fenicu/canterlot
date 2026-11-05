export interface DraftRecord {
  text: string;
  attachment_ids?: string[];
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
  put(
    text: string,
    attachmentIds: string[],
    baseRevision: number,
  ): Promise<DraftRecord>;
}

export interface DraftSyncOptions {
  deviceId: string;
  api: DraftSyncApi;
  local: DraftSyncLocal;
  current(): string;
  currentAttachments(): string[];
  apply(text: string, attachmentIds: string[]): void;
  now(): number;
  schedule(fn: () => void, ms: number): () => void;
  online(): boolean;
  debounceMs?: number;
  idleMs?: number;
}

const live = new Set<DraftSync>();

/**
 * Выход или очистка устройства: поле ввода сейчас опустеет, но общий черновик на хабе
 * принадлежит всем устройствам и стираться не должен.
 */
export function stopDraftSyncs(): void {
  for (const sync of [...live]) sync.stop();
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
  #savedAttachments: string[] = [];
  #dirty = false;
  #started = false;
  #lastEditAt = 0;
  #saveTimer: (() => void) | null = null;
  #idleTimer: (() => void) | null = null;
  #pending: DraftRecord | null = null;
  #saving = false;
  #queued = false;
  #stopped = false;

  constructor(options: DraftSyncOptions) {
    this.#options = options;
    this.#debounceMs = options.debounceMs ?? 500;
    this.#idleMs = options.idleMs ?? 2_000;
    live.add(this);
  }

  /** Больше ничего не писать на хаб: ни отложенную запись, ни уход с экрана. */
  stop(): void {
    this.#stopped = true;
    this.#cancelSave();
    this.#cancelIdle();
    this.#pending = null;
    live.delete(this);
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
    if (!this.#stopped) live.add(this);
    this.#revision = server?.revision ?? 0;
    this.#savedText = server?.text ?? "";
    const local = await this.#options.local.load();
    const attachments = local?.attachments ?? [];
    const localIds = attachments.map((item) => item.id);
    const serverText = server?.text ?? "";
    const serverIds = server?.attachment_ids ?? [];
    this.#savedAttachments = serverIds;
    const serverTime = server?.updated_at ? Date.parse(server.updated_at) : 0;
    const localTime = local?.updatedAt ?? 0;
    const localText = local?.text ?? "";
    const localDiffers =
      localText !== serverText || !sameIds(localIds, serverIds);

    if (local && localDiffers && localTime > serverTime) {
      this.#applyLocal(localText, localIds);
      return attachments;
    }
    if (server) {
      this.#options.apply(serverText, serverIds);
      this.#savedText = serverText;
      this.#savedAttachments = serverIds;
      this.#dirty = false;
      this.#options.local.set(serverText);
      return attachments.filter((item) => serverIds.includes(item.id));
    }
    if (localText || localIds.length > 0) this.#applyLocal(localText, localIds);
    return attachments;
  }

  /** Каждое нажатие: локальный буфер сразу, сервер через `debounceMs` после последнего. */
  typed(): void {
    this.#lastEditAt = this.#options.now();
    const text = this.#options.current();
    const attachmentIds = this.#options.currentAttachments();
    this.#dirty =
      text !== this.#savedText || !sameIds(attachmentIds, this.#savedAttachments);
    this.#options.local.set(text);
    if (this.#dirty) this.#scheduleSave();
    if (this.#pending) this.#scheduleIdle();
  }

  /** Чужое событие: эхо своего устройства и старые ревизии отбрасываются. */
  remote(record: DraftRecord | null): void {
    if (!record || !this.#started) return;
    if (record.device_id && record.device_id === this.#options.deviceId) return;
    if (record.revision <= this.#revision) return;
    this.#revision = record.revision;
    this.#savedText = record.text;
    this.#savedAttachments = record.attachment_ids ?? [];

    if (
      this.#options.current() === record.text &&
      sameIds(this.#options.currentAttachments(), this.#savedAttachments)
    ) {
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
    if (this.#stopped || !this.#dirty || !this.#options.online()) return;
    if (this.#saving) {
      this.#queued = true;
      return;
    }
    this.#saving = true;
    const text = this.#options.current();
    const attachmentIds = this.#options.currentAttachments();
    try {
      const record = await this.#options.api.put(
        text,
        attachmentIds,
        this.#revision,
      );
      this.#revision = Math.max(this.#revision, record.revision);
      this.#savedText = record.text;
      this.#savedAttachments = record.attachment_ids ?? attachmentIds;
      this.#dirty =
        this.#options.current() !== record.text ||
        !sameIds(this.#options.currentAttachments(), this.#savedAttachments);
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
    live.delete(this);
  }

  /** Ввод ушёл в очередь: локальная копия и отложенная запись больше не нужны. */
  sent(): void {
    this.#cancelSave();
    this.#cancelIdle();
    this.#pending = null;
    this.#dirty = false;
    this.#savedText = "";
    this.#savedAttachments = [];
    this.#options.local.clear();
  }

  #applyLocal(text: string, attachmentIds: string[]): void {
    this.#options.apply(text, attachmentIds);
    this.#options.local.set(text);
    this.#dirty =
      text !== this.#savedText ||
      !sameIds(attachmentIds, this.#savedAttachments);
    if (this.#dirty) this.#scheduleSave();
  }

  #applyRemote(record: DraftRecord): void {
    this.#cancelSave();
    this.#cancelIdle();
    this.#pending = null;
    this.#dirty = false;
    this.#savedText = record.text;
    this.#savedAttachments = record.attachment_ids ?? [];
    this.#revision = Math.max(this.#revision, record.revision);
    this.#options.apply(record.text, this.#savedAttachments);
    this.#options.local.set(record.text);
  }

  #scheduleSave(): void {
    this.#cancelSave();
    if (this.#stopped) return;
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

function sameIds(left: string[], right: string[]): boolean {
  return (
    left.length === right.length &&
    left.every((id, index) => id === right[index])
  );
}
