import { getRuntime } from '#lib/app/runtime.js';

/** Связи нет: сообщение ещё не ушло с устройства, обещать «Сохраняется» нечестно. */
export function waitingForLink(): boolean {
  if (typeof navigator !== 'undefined' && !navigator.onLine) return true;
  try {
    return getRuntime().connection.status.phase !== 'live';
  } catch {
    return false;
  }
}
