import type { OfficeKind } from '../attachments.js';
import type { OfficeReply, OfficeRequest } from './protocol.js';
import { officeProblem } from './sniff.js';

function spawn(kind: OfficeKind): Worker {
  // Два отдельных воркера, а не один с выбором внутри: каждая библиотека грузится только для своего формата.
  return kind === 'word'
    ? new Worker(new URL('./docx.worker.ts', import.meta.url), { type: 'module' })
    : new Worker(new URL('./xlsx.worker.ts', import.meta.url), { type: 'module' });
}

/**
 * Разбирает файл в фоновом потоке: главный поток только проверяет первые байты.
 * Отмена (закрытый просмотрщик) убивает воркер вместе с недоделанным разбором.
 */
export function parseOffice(kind: OfficeKind, bytes: Uint8Array<ArrayBuffer>, signal: AbortSignal): Promise<OfficeReply> {
  const problem = officeProblem(bytes);
  if (problem) return Promise.resolve({ kind: 'failed', reason: problem });
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const worker = spawn(kind);
    const finish = () => {
      worker.terminate();
      signal.removeEventListener('abort', abort);
    };
    const abort = () => {
      finish();
      reject(signal.reason);
    };
    signal.addEventListener('abort', abort, { once: true });
    worker.onmessage = (event: MessageEvent<OfficeReply>) => {
      finish();
      resolve(event.data);
    };
    // Упавший воркер (нехватка памяти, сбой библиотеки) — тот же «не удалось прочитать», не вечное «Открываю…».
    worker.onerror = (event) => {
      event.preventDefault();
      finish();
      resolve({ kind: 'failed', reason: 'corrupt' });
    };
    const buffer = bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength ? bytes.buffer : bytes.slice().buffer;
    const request: OfficeRequest = { bytes: buffer };
    worker.postMessage(request, [buffer]);
  });
}
