import { InstallController } from './install';
import { needsUpdate, UpdateController } from './update';

let install: InstallController | undefined;
export function installation(): InstallController {
  return install ??= new InstallController();
}

export async function register(env: { isSafe(): boolean; flushDrafts(): Promise<void> }): Promise<UpdateController | null> {
  if (!('serviceWorker' in navigator)) return null;
  await navigator.serviceWorker.register('/service-worker.js', { scope: '/', updateViaCache: 'none' });
  const reg = await navigator.serviceWorker.ready;
  const controller = new UpdateController(reg, { ...env, hidden: () => document.hidden });
  if (needsUpdate) controller.incompatible();
  return controller;
}
