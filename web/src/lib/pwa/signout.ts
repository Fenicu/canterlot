import type { Idb } from "#lib/storage/idb.js";
import { purgeAllStores, retireStorage } from "#lib/storage/idb.js";
import { retireOwner } from "#lib/storage/owner.js";
import { stopDraftWriters } from "#lib/drafts/store.js";
import { closeOpenJournals } from "#lib/ops/journal.js";
import { session } from "#lib/api/http.js";
import { httpHealth } from "#lib/api/gate.js";
import { resetConfig } from "#lib/api/config.js";
import { restoreComposerText } from "#lib/composer/restore.js";
import { staleness } from "./offline";

export async function purgeLocal(deps: {
  idb: Idb;
  caches: Pick<CacheStorage, "keys" | "delete">;
  ls: Storage;
  connection: { dispose(): void };
  runtime: { dispose(): void };
}): Promise<void> {
  session.state = "expired";
  restoreComposerText("");
  deps.connection.dispose();
  retireOwner();
  deps.runtime.dispose();
  stopDraftWriters();
  closeOpenJournals();
  staleness.reset();
  httpHealth.reset();
  resetConfig();
  await purgeAllStores();
  clearOwnerPreferences(deps.ls);
  const keys = await deps.caches.keys();
  await Promise.all(
    keys
      .filter((key) => !key.startsWith("canterlot-shell-"))
      .map((key) => deps.caches.delete(key)),
  );
  announceSignout();
}

const CHANNEL = "canterlot";
const OWNER_KEY = "canterlot:current_owner";
// Канал доставляет сообщение и слушателю этого же окна: своё объявление отличаем по метке.
const WINDOW_ID =
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : String(Math.random());

/** Сообщает остальным окнам приложения, что данные на устройстве удалены. */
export function announceSignout(): void {
  if (typeof BroadcastChannel === "undefined") return;
  const channel = new BroadcastChannel(CHANNEL);
  channel.postMessage({ type: "signout", from: WINDOW_ID });
  channel.close();
}

/**
 * Слушает выход в другом окне: сообщение BroadcastChannel или удаление ключа владельца
 * из localStorage (событие storage приходит только в чужие окна). Срабатывает один раз.
 */
export function watchSignout(onSignout: () => void): () => void {
  let fired = false;
  const fire = (): void => {
    if (fired) return;
    fired = true;
    onSignout();
  };
  const channel =
    typeof BroadcastChannel === "undefined"
      ? null
      : new BroadcastChannel(CHANNEL);
  const onMessage = (event: MessageEvent): void => {
    const data = event.data as { type?: unknown; from?: unknown } | null;
    if (data?.type === "signout" && data.from !== WINDOW_ID) fire();
  };
  const onStorage = (event: StorageEvent): void => {
    if (event.key === null || (event.key === OWNER_KEY && event.newValue === null))
      fire();
  };
  channel?.addEventListener("message", onMessage);
  window.addEventListener("storage", onStorage);
  return () => {
    channel?.removeEventListener("message", onMessage);
    channel?.close();
    window.removeEventListener("storage", onStorage);
  };
}

/** Окно узнало о выходе в другом окне: прекратить запись, дочистить хранилище и перезагрузиться. */
export async function followSignout(deps: {
  ls: Storage;
  reload(): void;
}): Promise<void> {
  restoreComposerText("");
  retireOwner();
  stopDraftWriters();
  closeOpenJournals();
  retireStorage();
  staleness.reset();
  httpHealth.reset();
  resetConfig();
  try {
    await purgeAllStores();
  } catch {
    // Хранилище уже очистило окно, где нажали «Выйти».
  }
  clearOwnerPreferences(deps.ls);
  deps.reload();
}

export function clearOwnerPreferences(storage: Storage): void {
  const keys = Array.from({ length: storage.length }, (_, i) => storage.key(i));
  for (const key of keys)
    if (key?.startsWith("canterlot.") || key?.startsWith("canterlot:"))
      storage.removeItem(key);
}
