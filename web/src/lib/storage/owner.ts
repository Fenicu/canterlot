import { purgeAllStores, openDb } from '$lib/storage/idb';
import { stopDraftWriters } from '$lib/drafts/store';
import { session } from '$lib/api/http';

const OWNER_STORAGE_KEY = 'canterlot:current_owner';

export async function bindOwner(
  meOrId: string | { owner_id: string }
): Promise<'first' | 'same' | 'changed'> {
  const newOwnerId = typeof meOrId === 'string' ? meOrId : meOrId.owner_id;
  let prevOwner: string | undefined | null = null;

  if (typeof localStorage !== 'undefined') {
    prevOwner = localStorage.getItem(OWNER_STORAGE_KEY);
  }

  if (!prevOwner) {
    const sysDb = await openDb('__system__');
    const stored = await sysDb.get<string>('kv', 'current_owner');
    if (stored) {
      prevOwner = stored;
    }
  }

  if (!prevOwner) {
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(OWNER_STORAGE_KEY, newOwnerId);
    }
    const sysDb = await openDb('__system__');
    await sysDb.put('kv', 'current_owner', newOwnerId);
    return 'first';
  }

  if (prevOwner === newOwnerId) {
    return 'same';
  }

  // Владелец изменился: остановить писателей, затем очистить IDB, кэши и черновики.
  session.state = 'expired';
  stopDraftWriters();
  await purgeAllStores();

  if (typeof caches !== 'undefined' && typeof caches.keys === 'function') {
    try {
      const cacheKeys = await caches.keys();
      await Promise.all(cacheKeys.map(k => caches.delete(k)));
    } catch {
      // Игнорируем ошибки доступа к ServiceWorker cache
    }
  }

  if (typeof localStorage !== 'undefined') {
    const toRemove: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (
        key?.startsWith('canterlot:draft:') ||
        key?.startsWith('canterlot.draft-tail.')
      ) {
        toRemove.push(key);
      }
    }
    for (const k of toRemove) {
      localStorage.removeItem(k);
    }
    localStorage.setItem(OWNER_STORAGE_KEY, newOwnerId);
  }

  const sysDb = await openDb('__system__');
  await sysDb.put('kv', 'current_owner', newOwnerId);
  session.state = 'ok';

  return 'changed';
}
