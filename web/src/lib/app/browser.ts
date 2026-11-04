import { resetConfig } from "#lib/api/config.js";
import { restoreComposerText } from "#lib/composer/restore.js";
import { createRuntime, getRuntime, type Runtime } from "#lib/app/runtime.js";
import { onOwnerRetired } from "#lib/storage/owner.js";

let pending: Promise<Runtime> | null = null;
let cleanupBound = false;
let current: Runtime | null = null;
const cleanups = new Set<() => void>();
const listeners = new Set<(runtime: Runtime) => void>();

/** Смена владельца: модуль отписывает своё состояние, не таща чат в оболочку. */
export function onOwnerCleanup(cleanup: () => void): void {
  cleanups.add(cleanup);
}

/** Готовый рантайм: чат привязывает к нему удаление сессий, как только подгрузится. */
export function onRuntimeReady(listener: (runtime: Runtime) => void): void {
  listeners.add(listener);
  if (current) listener(current);
}

function ready(runtime: Runtime): void {
  current = runtime;
  for (const listener of listeners) listener(runtime);
}

function bindCleanup(): void {
  if (cleanupBound) return;
  cleanupBound = true;
  onOwnerRetired(() => {
    restoreComposerText("");
    for (const cleanup of cleanups) cleanup();
    resetConfig();
    pending = null;
    current = null;
    cleanupBound = false;
  });
}

export function browserRuntime(): Promise<Runtime> {
  bindCleanup();
  try {
    const runtime = getRuntime();
    ready(runtime);
    return Promise.resolve(runtime);
  } catch {
    pending ??= createRuntime({
      fetch: globalThis.fetch.bind(globalThis),
      WebSocket: globalThis.WebSocket,
      indexedDB: globalThis.indexedDB,
      clock: { now: () => Date.now() },
      schedule: (fn, ms) => {
        const id = setTimeout(fn, ms);
        return () => clearTimeout(id);
      },
    });
    return pending
      .then((runtime) => {
        bindCleanup();
        ready(runtime);
        return runtime;
      })
      .catch((error) => {
        pending = null;
        throw error;
      });
  }
}
