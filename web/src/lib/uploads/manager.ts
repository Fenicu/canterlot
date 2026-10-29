import type { CanterlotDb } from "#lib/storage/idb.js";
import { uploadLimits } from "#lib/api/config.js";
import type { PatchSender } from "#lib/uploads/patch.js";
import { ApiError } from "#lib/api/http.js";
import {
  uploadFile,
  attachmentName,
  defaultAttachmentApi,
  UploadLimitError,
  type AttachmentApi,
} from "#lib/uploads/uploader.js";
import { browserHasher, type Hasher } from "#lib/uploads/hasher.js";

export { UploadLimitError };
export type { Hasher, PatchSender };

export type UploadPhase =
  | "queued"
  | "hashing"
  | "creating"
  | "uploading"
  | "completing"
  | "ready"
  | "failed"
  | "cancelled";

export interface Upload {
  id: string;
  conversationId: string;
  name: string;
  size: number;
  phase: UploadPhase;
  sent: number;
  attachmentId?: string;
  error?: { code: string; message: string; retryable: boolean };
}

interface StoredUpload {
  id: string;
  conversationId: string;
  name: string;
  size: number;
  phase: UploadPhase;
  sent: number;
  offset: number;
  createKey: string;
  completeKey: string;
  chunkBytes?: number;
  attachmentId?: string;
  sha256?: string;
  file: File;
  error?: Upload["error"];
}

const ACTIVE = new Set<UploadPhase>([
  "queued",
  "hashing",
  "creating",
  "uploading",
  "completing",
  "failed",
]);

export class UploadManager {
  readonly db: CanterlotDb;
  readonly concurrency = 2;

  #views = new Map<string, Upload>();
  #records = new Map<string, StoredUpload>();
  #limits: () => { maxBytes: number; maxPerInput: number; sliceBytes?: number };
  #api: AttachmentApi = defaultAttachmentApi;
  #patch?: PatchSender;
  #hasher: Hasher = browserHasher();
  #sliceBytes: number;
  #queue: StoredUpload[] = [];
  #active = 0;
  #controllers = new Map<string, AbortController>();
  #disposed = false;

  dispose(): void {
    this.#disposed = true;
    for (const controller of this.#controllers.values()) controller.abort();
    this.#queue = [];
    this.#records.clear();
    this.#views.clear();
  }

  constructor(deps: {
    api?: AttachmentApi;
    db: CanterlotDb;
    patch?: PatchSender;
    hasher?: Hasher;
    limits?: () => {
      maxBytes: number;
      maxPerInput: number;
      sliceBytes?: number;
    };
    sliceBytes?: number;
  }) {
    this.db = deps.db;
    if (deps.api) this.#api = deps.api;
    if (deps.patch) this.#patch = deps.patch;
    if (deps.hasher) this.#hasher = deps.hasher;
    const configured = uploadLimits();
    this.#sliceBytes = deps.sliceBytes ?? configured.sliceBytes;
    this.#limits = deps.limits ?? (() => configured);
  }

  add(conversationId: string, file: File, countInDraft: number): Upload {
    this.#assertLimits(file, countInDraft);
    const upload: Upload = {
      id: crypto.randomUUID(),
      conversationId,
      name: attachmentName(file.name),
      size: file.size,
      phase: "queued",
      sent: 0,
    };
    const record: StoredUpload = {
      ...upload,
      offset: 0,
      createKey: crypto.randomUUID(),
      completeKey: crypto.randomUUID(),
      chunkBytes: this.#sliceBytes,
      file,
    };
    this.#views.set(upload.id, upload);
    this.#records.set(upload.id, record);
    void this.#persist(record).then(() => this.#enqueue(record));
    return upload;
  }

  list(conversationId: string): readonly Upload[] {
    return [...this.#views.values()].filter(
      (item) => item.conversationId === conversationId,
    );
  }

  retry(id: string): void {
    const upload = this.#views.get(id);
    const record = this.#records.get(id);
    if (!upload || !record || upload.phase !== "failed" || !record.file) return;
    upload.phase = "queued";
    upload.error = undefined;
    record.phase = "queued";
    record.error = undefined;
    void this.#persist(record).then(() => this.#enqueue(record));
  }

  resumePending(): Promise<void> {
    return this.#resumeAll();
  }

  async cancel(id: string): Promise<void> {
    const upload = this.#views.get(id);
    const record = this.#records.get(id);
    this.#queue = this.#queue.filter((item) => item.id !== id);
    this.#controllers.get(id)?.abort();
    const remote = upload?.attachmentId ?? record?.attachmentId;
    if (remote) {
      try {
        await this.#api.deleteAttachment?.(remote);
      } catch {
        // Удаление на сервере не должно оставлять локальную строку.
      }
    }
    if (upload) upload.phase = "cancelled";
    this.#records.delete(id);
    this.#controllers.delete(id);
    await this.db.delete("uploads", id);
  }

  #assertLimits(file: File, countInDraft: number): void {
    const limits = this.#limits();
    if (file.size === 0) throw new UploadLimitError("empty");
    if (file.size > limits.maxBytes) throw new UploadLimitError("too_large");
    if (countInDraft + 1 > limits.maxPerInput)
      throw new UploadLimitError("too_many");
  }

  #enqueue(record: StoredUpload): void {
    if (this.#disposed) return;
    if (this.#queue.some((item) => item.id === record.id)) return;
    this.#queue.push(record);
    void this.#drain();
  }

  async #drain(): Promise<void> {
    while (this.#queue.length > 0 && this.#active < this.concurrency) {
      const record = this.#queue.shift();
      if (!record || !this.#records.has(record.id)) continue;
      this.#active += 1;
      void this.#run(record).finally(() => {
        this.#active -= 1;
        void this.#drain();
      });
    }
  }

  async #run(record: StoredUpload): Promise<void> {
    const view = this.#views.get(record.id);
    if (!view || view.phase === "cancelled") return;
    const controller = new AbortController();
    this.#controllers.set(record.id, controller);
    const api = this.#apiFor(controller.signal);
    try {
      const result = await uploadFile(record.file, record.conversationId, {
        api,
        hasher: this.#hasher,
        resumeAttachmentId: record.attachmentId,
        idempotencyKey: record.createKey,
        completeKey: record.completeKey,
        sliceBytes: record.chunkBytes ?? this.#sliceBytes,
        signal: controller.signal,
        onPhase: (phase) => {
          view.phase = phase;
          record.phase = phase;
          void this.#persist(record);
        },
        onProgress: (progress) => {
          view.sent = progress.uploaded;
          record.sent = progress.uploaded;
          record.offset = progress.uploaded;
        },
        onAttachmentCreated: (attachmentId, chunkBytes) => {
          view.attachmentId = attachmentId;
          record.attachmentId = attachmentId;
          record.chunkBytes = chunkBytes;
          void this.#persist(record);
        },
      });
      if (controller.signal.aborted) return;
      view.phase = "ready";
      view.sent = record.size;
      view.attachmentId = result.id;
      this.#records.delete(record.id);
      await this.db.delete("uploads", record.id);
    } catch (err) {
      if (
        controller.signal.aborted ||
        (err instanceof Error && /отмен/i.test(err.message))
      ) {
        view.phase = "cancelled";
        return;
      }
      const retryable =
        !(err instanceof ApiError) ||
        err.status >= 500 ||
        err.code === "too_many_requests" ||
        err.retry === "safe_same_key";
      view.phase = "failed";
      view.error = {
        code: err instanceof ApiError ? err.code : "upload",
        message: err instanceof Error ? err.message : "Загрузка не удалась",
        retryable,
      };
      record.phase = "failed";
      record.error = view.error;
      await this.#persist(record);
    } finally {
      this.#controllers.delete(record.id);
    }
  }

  #apiFor(signal: AbortSignal): AttachmentApi {
    const base = this.#api;
    const patch = this.#patch;
    return {
      ...base,
      patchContent: (id, chunk, offset, options) => {
        if (patch) {
          return patch(`/api/v1/attachments/${id}/content`, chunk, offset, {
            signal,
            onProgress: options?.onProgress,
          });
        }
        return base.patchContent(id, chunk, offset, { ...options, signal });
      },
    };
  }

  async #persist(record: StoredUpload): Promise<void> {
    if (!this.#records.has(record.id)) return;
    try {
      await this.db.put("uploads", record.id, record);
    } catch (err) {
      if (err instanceof Error && err.message === "storage_retired") return;
      throw err;
    }
  }

  async #resumeAll(): Promise<void> {
    const records = await this.db.list<StoredUpload>("uploads");
    if (this.#disposed) return;
    for (const record of records) {
      if (!record?.id || !record.file || !ACTIVE.has(record.phase)) continue;
      if (!this.#views.has(record.id)) {
        this.#views.set(record.id, {
          id: record.id,
          conversationId: record.conversationId,
          name: record.name,
          size: record.size,
          phase: record.phase,
          sent: record.sent ?? record.offset ?? 0,
          attachmentId: record.attachmentId,
          error: record.error,
        });
      }
      this.#records.set(record.id, record);
      this.#enqueue(record);
    }
    await this.#idle();
  }

  #idle(): Promise<void> {
    return new Promise((resolve) => {
      const look = () => {
        if (this.#active === 0 && this.#queue.length === 0) resolve();
        else setTimeout(look, 10);
      };
      look();
    });
  }
}
