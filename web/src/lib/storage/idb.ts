export type StoreName = 'drafts' | 'ops' | 'uploads' | 'share_inbox' | 'kv';
export type Store = StoreName;
export type Idb = CanterlotDb;

const DB_NAME = 'canterlot';
const DB_VERSION = 1;
const STORE_NAMES: StoreName[] = ['drafts', 'ops', 'uploads', 'share_inbox', 'kv'];

let generation = 0;
const connections = new Set<IDBDatabase>();

export function retireStorage(): void {
  generation += 1;
  for (const connection of connections) {
    try {
      connection.close();
    } catch {
      // Соединение уже закрыто.
    }
  }
  connections.clear();
}

export interface CanterlotDb {
  readonly ownerId: string;
  get<T = unknown>(store: StoreName, key: string): Promise<T | undefined>;
  put<T = unknown>(store: StoreName, key: string, value: T): Promise<void>;
  delete(store: StoreName, key: string): Promise<void>;
  list<T = unknown>(store: StoreName, prefix?: string): Promise<T[]>;
  clear(store?: StoreName): Promise<void>;
}

function openRawDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onupgradeneeded = () => {
      const db = request.result;
      for (const store of STORE_NAMES) {
        if (!db.objectStoreNames.contains(store)) {
          db.createObjectStore(store);
        }
      }
    };

    request.onsuccess = () => {
      const opened = request.result;
      connections.add(opened);
      opened.onclose = () => connections.delete(opened);
      resolve(opened);
    };
    request.onerror = () => reject(request.error);
  });
}

export async function purgeAllStores(): Promise<void> {
  retireStorage();
  const db = await openRawDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE_NAMES, 'readwrite');
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);

    for (const store of STORE_NAMES) {
      tx.objectStore(store).clear();
    }
  });
}

function commit(db: IDBDatabase, store: StoreName, work: (os: IDBObjectStore) => void): Promise<void> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(store, 'readwrite');
    let settled = false;
    const fail = () => {
      if (settled) return;
      settled = true;
      reject(tx.error ?? new DOMException('The transaction was aborted', 'AbortError'));
    };
    tx.oncomplete = () => {
      if (settled) return;
      settled = true;
      resolve();
    };
    tx.onabort = fail;
    tx.onerror = fail;
    work(tx.objectStore(store));
  });
}

export async function openDb(ownerId: string): Promise<CanterlotDb> {
  const db = await openRawDb();
  const born = generation;
  const live = (): void => {
    if (born !== generation) throw new Error('storage_retired');
  };

  function scopedKey(key: string): string {
    return `${ownerId}/${key}`;
  }

  return {
    ownerId,

    async get<T = unknown>(store: StoreName, key: string): Promise<T | undefined> {
      live();
      return new Promise((resolve, reject) => {
        const tx = db.transaction(store, 'readonly');
        const os = tx.objectStore(store);
        const req = os.get(scopedKey(key));
        req.onsuccess = () => resolve(req.result as T | undefined);
        req.onerror = () => reject(req.error);
      });
    },

    async put<T = unknown>(store: StoreName, key: string, value: T): Promise<void> {
      live();
      return commit(db, store, (os) => {
        os.put(value, scopedKey(key));
      });
    },

    async delete(store: StoreName, key: string): Promise<void> {
      live();
      return commit(db, store, (os) => {
        os.delete(scopedKey(key));
      });
    },

    async list<T = unknown>(store: StoreName, prefix = ''): Promise<T[]> {
      live();
      return new Promise((resolve, reject) => {
        const tx = db.transaction(store, 'readonly');
        const os = tx.objectStore(store);
        const lower = scopedKey(prefix);
        const upper = `${lower}\uffff`;
        const range = IDBKeyRange.bound(lower, upper);
        const req = os.openCursor(range);
        const results: T[] = [];

        req.onsuccess = () => {
          const cursor = req.result;
          if (cursor) {
            results.push(cursor.value as T);
            cursor.continue();
          } else {
            resolve(results);
          }
        };
        req.onerror = () => reject(req.error);
      });
    },

    async clear(store?: StoreName): Promise<void> {
      live();
      const names = store ? [store] : [...STORE_NAMES];
      for (const name of names) {
        await commit(db, name, (os) => {
          const lower = `${ownerId}/`;
          const upper = `${lower}\uffff`;
          const req = os.openCursor(IDBKeyRange.bound(lower, upper));
          req.onsuccess = () => {
            const cursor = req.result;
            if (cursor) {
              cursor.delete();
              cursor.continue();
            }
          };
        });
      }
    }
  };
}
