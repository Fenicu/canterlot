import { ApiError, NetworkError } from "#lib/api/http.js";
import {
  completeAttachment as completeAttachmentRequest,
  createAttachment as createAttachmentRequest,
  deleteAttachment as deleteAttachmentRequest,
  headAttachmentContent,
} from "#lib/api/client.js";
import type { components } from "#lib/api/generated/openapi.js";
import { patchChunk } from "#lib/uploads/patch.js";
import { browserHasher, type Hasher } from "#lib/uploads/hasher.js";

export type AttachmentRecord = components["schemas"]["AttachmentRecord"];
export type CreatedAttachment = components["schemas"]["CreatedAttachment"];

export function attachmentName(name: string): string {
  const base = name.split(/[/\\]/).pop() || "file";
  const bytes = new TextEncoder().encode(base);
  if (bytes.length <= 255) return base;
  let end = 255;
  while (end > 0 && ((bytes[end] ?? 0) & 0xc0) === 0x80) end--;
  return new TextDecoder().decode(bytes.subarray(0, end));
}

export class UploadLimitError extends Error {
  readonly reason: "too_large" | "too_many";

  constructor(reason: "too_large" | "too_many") {
    const message =
      reason === "too_large"
        ? "Файл больше 100 МиБ"
        : "Не более 10 файлов в одном сообщении";
    super(message);
    this.name = "UploadLimitError";
    this.reason = reason;
  }
}

export interface AttachmentApi {
  createAttachment(
    body: components["schemas"]["NewAttachment"],
    options?: { idempotencyKey?: string },
  ): Promise<Pick<CreatedAttachment, "id" | "state" | "upload">>;
  headContent(id: string): Promise<{ offset: number; length: number }>;
  patchContent(
    id: string,
    chunk: Blob,
    offset: number,
    options?: {
      signal?: AbortSignal;
      onProgress?: (loaded: number, total: number) => void;
    },
  ): Promise<{ offset: number }>;
  completeAttachment(
    id: string,
    body?: components["schemas"]["CompleteBody"],
    options?: { idempotencyKey?: string },
  ): Promise<Pick<AttachmentRecord, "id" | "state">>;
  deleteAttachment?(id: string): Promise<void>;
}

export const defaultAttachmentApi: AttachmentApi = {
  async createAttachment(body, options) {
    const key = options?.idempotencyKey ?? crypto.randomUUID();
    const res = await createAttachmentRequest(body, key);
    return res.data;
  },

  async headContent(id: string) {
    const res = await headAttachmentContent(id);
    const offsetHeader = res.headers.get("Upload-Offset");
    const lengthHeader = res.headers.get("Upload-Length");
    return {
      offset: offsetHeader ? parseInt(offsetHeader, 10) : 0,
      length: lengthHeader ? parseInt(lengthHeader, 10) : 0,
    };
  },

  async patchContent(
    id: string,
    chunk: Blob,
    offset: number,
    options?: { signal?: AbortSignal },
  ) {
    return patchChunk(`/api/v1/attachments/${id}/content`, chunk, offset, {
      signal: options?.signal,
    });
  },

  async completeAttachment(id: string, body, options) {
    const key = options?.idempotencyKey ?? crypto.randomUUID();
    const res = await completeAttachmentRequest(id, body ?? {}, key);
    return res.data;
  },

  async deleteAttachment(id: string) {
    await deleteAttachmentRequest(id, crypto.randomUUID());
  },
};

export interface UploadFileOptions {
  api?: AttachmentApi;
  hasher?: Hasher;
  resumeAttachmentId?: string;
  idempotencyKey?: string;
  completeKey?: string;
  sliceBytes?: number;
  retryDelayMs?: number;
  currentDraftCount?: number;
  onProgress?: (p: { uploaded: number; total: number }) => void;
  onPhase?: (
    phase: "hashing" | "creating" | "uploading" | "completing",
  ) => void;
  signal?: AbortSignal;
  onAttachmentCreated?: (id: string, chunkBytes: number) => void;
}

export interface UploadFileResult {
  id: string;
  status: string;
  sha256: string;
  size: number;
}

export async function uploadFile(
  file: File,
  conversationId: string,
  options?: UploadFileOptions,
): Promise<UploadFileResult> {
  // 1. Валидация. Пустой файл законен: тел PATCH нет, хаб публикует его при завершении.
  if (file.size > 104857600) {
    throw new UploadLimitError("too_large");
  }
  if (
    options?.currentDraftCount !== undefined &&
    options.currentDraftCount + 1 > 10
  ) {
    throw new UploadLimitError("too_many");
  }

  const api = options?.api ?? defaultAttachmentApi;
  const hasher = options?.hasher ?? browserHasher();
  const retryDelay = options?.retryDelayMs ?? 1000;
  const signal = options?.signal;
  const createKey = options?.idempotencyKey ?? crypto.randomUUID();
  const finishKey = options?.completeKey ?? crypto.randomUUID();

  options?.onPhase?.("hashing");
  const digest = await hasher.sha256(
    file,
    () => undefined,
    signal ?? new AbortController().signal,
  );
  throwIfAborted(signal);

  let attachmentId = options?.resumeAttachmentId;
  let chunkBytes = options?.sliceBytes ?? 8 * 1024 * 1024;

  if (!attachmentId) {
    options?.onPhase?.("creating");
    const regRes = await retrying(
      () =>
        api.createAttachment(
          {
            conversation_id: conversationId,
            name: attachmentName(file.name),
            mime: file.type || "application/octet-stream",
            size: file.size,
            sha256: digest,
          },
          { idempotencyKey: createKey },
        ),
      retryDelay,
      signal,
    );
    attachmentId = regRes.id;
    if (regRes.upload?.upload_chunk_bytes)
      chunkBytes = regRes.upload.upload_chunk_bytes;
  }
  options?.onAttachmentCreated?.(attachmentId, chunkBytes);
  options?.onPhase?.("uploading");
  let offset = (await readHead(api, attachmentId, retryDelay, signal)).offset;

  // 3. Цикл передачи слайсов
  while (offset < file.size) {
    throwIfAborted(signal);

    const nextEnd = Math.min(offset + chunkBytes, file.size);
    const slice = file.slice(offset, nextEnd);

    try {
      const patchRes = await api.patchContent(attachmentId, slice, offset, {
        signal,
      });
      offset = patchRes.offset;
      options?.onProgress?.({ uploaded: offset, total: file.size });
    } catch (err) {
      if (aborted(err, signal)) throw new Error("Загрузка отменена");
      if (err instanceof ApiError && err.code === "offset_mismatch") {
        const fromHeader = err.headers?.get("Upload-Offset");
        offset = fromHeader
          ? parseInt(fromHeader, 10)
          : (await readHead(api, attachmentId, retryDelay, signal)).offset;
        continue;
      }
      if (err instanceof ApiError && err.code === "upload_in_progress") {
        await pause(retryDelay, signal);
        offset = (await readHead(api, attachmentId, retryDelay, signal)).offset;
        continue;
      }
      if (err instanceof ApiError && err.code === "too_many_requests") {
        await pause(retryDelay, signal);
        continue;
      }
      if (transient(err)) {
        await pause(retryDelay, signal);
        offset = (await readHead(api, attachmentId, retryDelay, signal)).offset;
        continue;
      }
      throw err;
    }
  }
  if (file.size === 0) options?.onProgress?.({ uploaded: 0, total: 0 });

  options?.onPhase?.("completing");
  while (true) {
    try {
      const completeRes = await api.completeAttachment(
        attachmentId,
        { sha256: digest },
        { idempotencyKey: finishKey },
      );
      return {
        id: attachmentId,
        status: completeRes.state,
        sha256: digest,
        size: file.size,
      };
    } catch (err) {
      if (aborted(err, signal)) throw new Error("Загрузка отменена");
      if (
        transient(err) ||
        (err instanceof ApiError && err.code === "too_many_requests")
      ) {
        await pause(retryDelay, signal);
        continue;
      }
      throw err;
    }
  }
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw new Error("Загрузка отменена");
}

function aborted(err: unknown, signal?: AbortSignal): boolean {
  return (
    signal?.aborted === true ||
    (err instanceof DOMException && err.name === "AbortError") ||
    (err instanceof Error && /отмен/i.test(err.message))
  );
}

function transient(err: unknown): boolean {
  if (err instanceof NetworkError) return true;
  if (!(err instanceof ApiError)) return false;
  if (err.code === "too_many_requests") return true;
  return err.status >= 500 || err.retry === "safe_same_key";
}

async function pause(ms: number, signal?: AbortSignal): Promise<void> {
  throwIfAborted(signal);
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(new Error("Загрузка отменена"));
      },
      { once: true },
    );
  });
}

async function retrying<T>(
  work: () => Promise<T>,
  delay: number,
  signal?: AbortSignal,
): Promise<T> {
  for (;;) {
    try {
      return await work();
    } catch (err) {
      if (aborted(err, signal) || !transient(err))
        throw aborted(err, signal) ? new Error("Загрузка отменена") : err;
      await pause(delay, signal);
    }
  }
}

async function readHead(
  api: AttachmentApi,
  id: string,
  delay: number,
  signal?: AbortSignal,
): Promise<{ offset: number; length: number }> {
  for (;;) {
    try {
      return await api.headContent(id);
    } catch (err) {
      if (aborted(err, signal)) throw new Error("Загрузка отменена");
      if (
        err instanceof ApiError &&
        (err.code === "upload_in_progress" || transient(err))
      ) {
        await pause(delay, signal);
        continue;
      }
      if (err instanceof NetworkError) {
        await pause(delay, signal);
        continue;
      }
      throw err;
    }
  }
}
