import { InstallController } from './install';
import { needsUpdate, UpdateController } from './update';

let update: UpdateController | null = null;
export function currentUpdate(): UpdateController | null { return update; }

let install: InstallController | undefined;
export function installation(): InstallController {
  return install ??= new InstallController();
}

/**
 * Старый service worker и его оболочка больше не нужны: снимаем регистрацию и кэши
 * на каждом старте. Второй путь — аварийный worker по тому же URL.
 */
export async function removeInstalledWorkers(): Promise<void> {
  if (typeof navigator !== 'undefined' && 'serviceWorker' in navigator) {
    try {
      const registrations = await navigator.serviceWorker.getRegistrations();
      await Promise.all(registrations.map(registration => registration.unregister()));
    } catch { /* service worker мог быть уже снят */ }
  }
  if (typeof caches !== 'undefined') {
    try {
      const keys = await caches.keys();
      await Promise.all(keys.map(key => caches.delete(key)));
    } catch { /* хранилище кэшей недоступно */ }
  }
}

export async function register(env: { isSafe(): boolean; flushDrafts(): Promise<void>; version: string }): Promise<UpdateController> {
  await removeInstalledWorkers();
  const controller = new UpdateController({ ...env, hidden: () => document.hidden });
  if (needsUpdate) controller.incompatible();
  update = controller;
  return controller;
}
