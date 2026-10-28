import { sha256 as nobleSha256 } from '@noble/hashes/sha2.js';
import { bytesToHex } from '@noble/hashes/utils.js';

export interface HashOptions {
  chunkSize?: number;
  onProgress?: (done: number) => void;
  signal?: AbortSignal;
}

/**
 * Потоковое вычисление SHA-256 файла слайсами по 1 МиБ.
 * Никогда не держит весь файл в памяти целиком.
 */
export async function hashFileStreaming(
  file: Blob | File,
  options?: HashOptions
): Promise<{ sha256: string; size: number }> {
  const chunkSize = options?.chunkSize ?? 1024 * 1024;
  const hasher = nobleSha256.create();
  let offset = 0;

  while (offset < file.size) {
    if (options?.signal?.aborted) {
      throw new DOMException('The operation was aborted', 'AbortError');
    }
    const end = Math.min(offset + chunkSize, file.size);
    const slice = file.slice(offset, end);
    const buffer = await slice.arrayBuffer();
    hasher.update(new Uint8Array(buffer));
    offset = end;
    options?.onProgress?.(offset);
  }

  const hex = bytesToHex(hasher.digest());
  return { sha256: hex, size: file.size };
}

export interface Hasher {
  sha256(blob: Blob, onProgress: (done: number) => void, signal: AbortSignal): Promise<string>;
}

export function sha256(blob: Blob, onProgress: (done: number) => void, signal: AbortSignal): Promise<string> {
  return hashFileStreaming(blob, { onProgress, signal }).then((result) => result.sha256);
}

export function hashInWorker(blob: Blob, onProgress: (done: number) => void, signal: AbortSignal): Promise<string> {
  const worker = new Worker(new URL('./hash.worker.ts', import.meta.url), { type: 'module' });
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      signal.removeEventListener('abort', onAbort);
      worker.terminate();
      fn();
    };
    const onAbort = () => {
      worker.postMessage({ type: 'cancel' });
      finish(() => reject(new DOMException('The operation was aborted', 'AbortError')));
    };
    if (signal.aborted) {
      onAbort();
      return;
    }
    signal.addEventListener('abort', onAbort, { once: true });
    worker.onmessage = (event: MessageEvent<{ type: string; done?: number; sha256?: string; message?: string }>) => {
      if (event.data.type === 'progress' && event.data.done !== undefined) onProgress(event.data.done);
      if (event.data.type === 'done' && event.data.sha256) finish(() => resolve(event.data.sha256!));
      if (event.data.type === 'error') finish(() => reject(new Error(event.data.message ?? 'hash_failed')));
      if (event.data.type === 'cancelled') finish(() => reject(new DOMException('The operation was aborted', 'AbortError')));
    };
    worker.onerror = (event) => finish(() => reject(new Error(event.message || 'worker_failed')));
    worker.postMessage({ type: 'hash', file: blob });
  });
}

export function browserHasher(): Hasher {
  return {
    sha256(blob, onProgress, signal) {
      return typeof Worker === 'function' ? hashInWorker(blob, onProgress, signal) : sha256(blob, onProgress, signal);
    }
  };
}
