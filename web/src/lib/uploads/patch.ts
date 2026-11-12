import {
  ApiError,
  AuthRequired,
  envelopeMissing,
  envelopeOf,
  HUB_UNAVAILABLE,
  NetworkError,
  session,
  signOutOf,
} from "#lib/api/http.js";

export interface PatchChunkOptions {
  progressTimeoutMs?: number;
  onProgress?: (loaded: number, total: number) => void;
  signal?: AbortSignal;
}

export interface PatchChunkResult {
  offset: number;
}

export interface PatchSender {
  (
    url: string,
    chunk: Blob | Uint8Array,
    offset: number,
    options?: PatchChunkOptions,
  ): Promise<PatchChunkResult>;
}

/**
 * Отправляет часть вложения через XMLHttpRequest для поддержки upload.onprogress.
 * Контролирует таймаут прогресса (сбрасывается при каждом событии progress).
 */
export function patchChunk(
  url: string,
  chunk: Blob | Uint8Array,
  offset: number,
  options?: PatchChunkOptions,
): Promise<PatchChunkResult> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const timeoutMs = options?.progressTimeoutMs ?? 30000;
    let progressTimer: ReturnType<typeof setTimeout> | null = null;
    let isProgressTimedOut = false;

    function resetTimer() {
      if (progressTimer !== null) {
        clearTimeout(progressTimer);
      }
      progressTimer = setTimeout(() => {
        isProgressTimedOut = true;
        xhr.abort();
      }, timeoutMs);
    }

    function cleanup() {
      if (progressTimer !== null) {
        clearTimeout(progressTimer);
        progressTimer = null;
      }
      if (options?.signal) {
        options.signal.removeEventListener("abort", onAbortSignal);
      }
    }

    function onAbortSignal() {
      xhr.abort();
    }

    if (options?.signal) {
      if (options.signal.aborted) {
        reject(new Error("Запрос отменён"));
        return;
      }
      options.signal.addEventListener("abort", onAbortSignal);
    }

    xhr.upload.addEventListener("progress", (e) => {
      resetTimer();
      if (e.lengthComputable) {
        options?.onProgress?.(e.loaded, e.total);
      }
    });

    xhr.addEventListener("load", () => {
      cleanup();

      const rawHeaders = xhr.getAllResponseHeaders?.() ?? "";
      const headers = new Headers();
      for (const line of rawHeaders.split("\r\n")) {
        const idx = line.indexOf(":");
        if (idx > 0) {
          headers.set(
            line.slice(0, idx).trim().toLowerCase(),
            line.slice(idx + 1).trim(),
          );
        }
      }

      const offsetHeader = xhr.getResponseHeader("Upload-Offset");
      if (offsetHeader) {
        headers.set("upload-offset", offsetHeader);
      }

      const contentType = xhr.getResponseHeader("Content-Type") ?? "";
      if (xhr.status === 401) {
        let body: unknown = null;
        try {
          body = JSON.parse(xhr.responseText);
        } catch {
          body = null;
        }
        const out = signOutOf(body) ?? { code: "unauthenticated" as const };
        session.expire(out.code, out.reason);
        reject(new AuthRequired(out.code, out.reason));
        return;
      }

      // Переадресация, обрыв без статуса или HTML на адрес API: хаба за прокси нет.
      const landed = xhr.responseURL;
      if (landed) {
        const base =
          typeof window !== "undefined"
            ? window.location.href
            : "https://canterlot.local";
        const requested = new URL(url, base);
        const actual = new URL(landed, base);
        if (
          actual.origin !== requested.origin ||
          actual.pathname !== requested.pathname
        ) {
          reject(new NetworkError(HUB_UNAVAILABLE));
          return;
        }
      }

      if (xhr.status === 0 || contentType.includes("text/html")) {
        reject(new NetworkError(HUB_UNAVAILABLE));
        return;
      }

      if (xhr.status === 200 || xhr.status === 204) {
        if (offsetHeader === null || !/^\d+$/.test(offsetHeader)) {
          reject(new NetworkError("missing_upload_offset"));
          return;
        }
        resolve({ offset: Number(offsetHeader) });
        return;
      }

      let errorBody: ReturnType<typeof envelopeOf>;
      try {
        errorBody = envelopeOf(JSON.parse(xhr.responseText));
      } catch {
        errorBody = undefined;
      }
      if (!errorBody) {
        reject(envelopeMissing(xhr.status));
        return;
      }

      reject(
        new ApiError(
          xhr.status,
          errorBody.code,
          errorBody.message,
          errorBody.scope,
          errorBody.retry,
          errorBody.correlation_id,
          errorBody.details,
          headers,
        ),
      );
    });

    xhr.addEventListener("error", () => {
      cleanup();
      reject(new NetworkError("Сетевая ошибка при передаче части файла"));
    });

    xhr.addEventListener("abort", () => {
      cleanup();
      if (isProgressTimedOut) {
        // Застой связи (смена Wi-Fi на сотовую): загрузчик продолжит с принятого хабом смещения.
        reject(new NetworkError("Превышен таймаут прогресса загрузки"));
      } else {
        reject(new Error("Загрузка отменена"));
      }
    });

    xhr.open("PATCH", url);
    xhr.setRequestHeader("Upload-Offset", String(offset));
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.setRequestHeader("X-Canterlot-Client", "1");
    xhr.setRequestHeader("X-Correlation-Id", crypto.randomUUID());

    resetTimer();

    const body =
      chunk instanceof Blob
        ? chunk
        : (chunk as unknown as XMLHttpRequestBodyInit);
    xhr.send(body as XMLHttpRequestBodyInit);
  });
}
