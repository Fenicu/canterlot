import { InstallController } from './install';
import { needsUpdate, UpdateController } from './update';

let update: UpdateController | null = null;
export function currentUpdate(): UpdateController | null { return update; }

let install: InstallController | undefined;
export function installation(): InstallController {
  return install ??= new InstallController();
}

/** Постоянного service worker нет: приложение только следит за версией сборки. */
export async function register(env: { isSafe(): boolean; editing?(): boolean; flushDrafts(): Promise<void>; version: string }): Promise<UpdateController> {
  const controller = new UpdateController({ ...env, hidden: () => document.hidden });
  if (needsUpdate) controller.incompatible();
  update = controller;
  return controller;
}
