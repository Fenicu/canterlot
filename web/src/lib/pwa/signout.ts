import type { Idb } from "#lib/storage/idb.js";
import { purgeAllStores } from "#lib/storage/idb.js";
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
  clearOwnerPreferences(deps.ls);
  await purgeAllStores();
  const keys = await deps.caches.keys();
  await Promise.all(
    keys
      .filter((key) => !key.startsWith("canterlot-shell-"))
      .map((key) => deps.caches.delete(key)),
  );
}

export function clearOwnerPreferences(storage: Storage): void {
  const keys = Array.from({ length: storage.length }, (_, i) => storage.key(i));
  for (const key of keys)
    if (key?.startsWith("canterlot.") || key?.startsWith("canterlot:"))
      storage.removeItem(key);
}
