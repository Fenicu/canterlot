export interface DraftRecord {
  text: string;
  attachment_ids?: string[];
  revision: number;
  updated_at?: string | null;
  device_id?: string | null;
}

export interface DraftLocalRecord {
  text: string;
  updatedAt: number;
  attachments?: { id: string; name?: string; size?: number }[];
  /** Ревизия сервера, на которой локальный буфер был последний раз синхронизирован. */
  baseRevision?: number;
  /** Текст менялся после этой синхронизации: это оффлайн-правка владельца. */
  unsynced?: boolean;
}

export interface DraftLocalEdit {
  baseRevision?: number;
  unsynced?: boolean;
  attachments?: { id: string; name?: string; size?: number }[];
}

export interface DraftSyncLocal {
  load(): Promise<DraftLocalRecord | null>;
  set(text: string, edit?: DraftLocalEdit): void;
  clear(): void;
}

export interface DraftSyncApi {
  put(
    text: string,
    attachmentIds: string[],
    baseRevision: number,
  ): Promise<DraftRecord>;
  /** Авторитетная копия, когда снимок ещё не донёс черновик на экран. */
  get?(): Promise<DraftRecord>;
}

/** Хаб отверг запись: его текущий черновик приходит в `record`. */
export class DraftConflict extends Error {
  readonly record: DraftRecord;

  constructor(record: DraftRecord) {
    super("Черновик уже изменён с другого устройства");
    this.name = "DraftConflict";
    this.record = record;
  }
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
 * (DraftStore) остаётся буфером на случай офлайна и помнит ревизию, на которой
 * она синхронизировалась, и признак правок после неё. Выбор копии идёт по
 * ревизиям, не по часам: буфер без оффлайн-правок уступает серверу, даже пустому.
 * Эхо собственной записи отбрасывается по `device_id`, чужое применяется сразу,
 * если владелец не печатает, иначе — после паузы в `idleMs`, и только пока хаб
 * не принял более новую ревизию: своя запись, успевшая лечь поверх, её отменяет.
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

  /** Выбирает копию по ревизиям: без оффлайн-правок побеждает сервер, даже пустой. */
  async start(server: DraftRecord | null): Promise<{
    id: string;
    name?: string;
    size?: number;
  }[]> {
    if (this.#started) return [];
    this.#started = true;
    if (!this.#stopped) live.add(this);
    const local = await this.#options.local.load();
    const attachments = local?.attachments ?? [];
    const localIds = attachments.map((item) => item.id);
    const localText = local?.text ?? "";

    if (local?.unsynced) {
      // Реальная оффлайн-правка: она сделана на своей ревизии и может быть
      // отправлена только с ней, иначе хаб вернёт конфликт.
      const base = local.baseRevision ?? 0;
      this.#savedText = server?.text ?? "";
      this.#savedAttachments = server?.attachment_ids ?? [];
      this.#lastEditAt = this.#options.now();
      this.#applyLocal(localText, localIds, base);
      return attachments;
    }

    // Снимок может ещё не нести черновик: запросим хаб напрямую, чтобы не
    // решить по отсутствию записи то, что на самом деле сохранилось на сервере.
    let resolved = server;
    if (!resolved && this.#options.api.get) {
      try {
        resolved = await this.#options.api.get();
      } catch {
        // Без связи оставляем уже синхронизированный буфер, но ничего не пишем.
        if (local) {
          this.#revision = local.baseRevision ?? 0;
          this.#savedText = localText;
          this.#savedAttachments = localIds;
          this.#options.apply(localText, localIds);
          this.#options.local.set(localText, {
            baseRevision: this.#revision,
            unsynced: false,
            attachments,
          });
        }
        return attachments;
      }
    }

    const serverText = resolved?.text ?? "";
    const serverIds = resolved?.attachment_ids ?? [];
    const serverRevision = resolved?.revision ?? 0;

    // Старый буфер без правок не воскрешает удалённый черновик: берём сервер,
    // в том числе пустой, и стираем давний текст в локальном хранилище.
    // Метаданные лотка берём из локального буфера: новые вложения дотянет
    // `#reconcileAttachments` через `attachmentInfo`, а не пустой объект.
    const restored = attachments.filter((item) => serverIds.includes(item.id));
    this.#revision = serverRevision;
    this.#options.apply(serverText, serverIds);
    this.#savedText = serverText;
    this.#savedAttachments = serverIds;
    this.#dirty = false;
    this.#pending = null;
    this.#options.local.set(serverText, {
      baseRevision: serverRevision,
      unsynced: false,
      attachments: restored,
    });
    return restored;
  }

  /** Каждое нажатие: локальный буфер сразу, сервер через `debounceMs` после последнего. */
  typed(): void {
    this.#lastEditAt = this.#options.now();
    const text = this.#options.current();
    const attachmentIds = this.#options.currentAttachments();
    this.#dirty =
      text !== this.#savedText || !sameIds(attachmentIds, this.#savedAttachments);
    this.#options.local.set(text, {
      baseRevision: this.#revision,
      unsynced: this.#dirty,
    });
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
      this.#options.local.set(record.text, {
        baseRevision: record.revision,
        unsynced: false,
        attachments: (record.attachment_ids ?? []).map((id) => ({ id })),
      });
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
      this.#dropStalePending();
      this.#savedText = record.text;
      this.#savedAttachments = record.attachment_ids ?? attachmentIds;
      this.#dirty =
        this.#options.current() !== record.text ||
        !sameIds(this.#options.currentAttachments(), this.#savedAttachments);
      if (this.#dirty) this.#scheduleSave();
      else
        this.#options.local.set(record.text, {
          baseRevision: record.revision,
          unsynced: false,
          attachments: (record.attachment_ids ?? attachmentIds).map((id) => ({ id })),
        });
    } catch (err) {
      if (err instanceof DraftConflict) {
        const record = err.record;
        this.#revision = Math.max(this.#revision, record.revision);
        this.#dropStalePending();
        this.#savedText = record.text;
        this.#savedAttachments = record.attachment_ids ?? [];
        const rebase =
          text !== record.text ||
          !sameIds(attachmentIds, this.#savedAttachments);
        if (!rebase) {
          // После синхронизации правок нет: целиком берём серверную копию.
          this.#applyRemote(record);
        } else {
          // Правка владельца остаётся, но повтор идёт уже на новой базе.
          this.#dirty = true;
          this.#options.local.set(text, {
            baseRevision: record.revision,
            unsynced: true,
          });
          this.#scheduleSave();
        }
        return;
      }
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

  #applyLocal(text: string, attachmentIds: string[], baseRevision: number): void {
    this.#revision = baseRevision;
    this.#options.apply(text, attachmentIds);
    this.#dirty =
      text !== this.#savedText ||
      !sameIds(attachmentIds, this.#savedAttachments);
    this.#options.local.set(text, {
      baseRevision,
      unsynced: this.#dirty,
    });
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
    this.#options.local.set(record.text, {
      baseRevision: this.#revision,
      unsynced: false,
      attachments: this.#savedAttachments.map((id) => ({ id })),
    });
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
      this.#dropStalePending();
      const record = this.#pending;
      if (record) this.#applyRemote(record);
    }, wait);
  }

  /**
   * Отложенная чужая правка старше того, что хаб уже принял или вернул: поверх более новой
   * ревизии она не встаёт, иначе поле ввода и хаб разошлись бы без признака несохранённого.
   */
  #dropStalePending(): void {
    if (!this.#pending || this.#pending.revision >= this.#revision) return;
    this.#pending = null;
    this.#cancelIdle();
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
