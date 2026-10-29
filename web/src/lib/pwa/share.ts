import type { Idb } from '$lib/storage/idb';

export interface ShareItem { id: string; receivedAt: number; files: File[]; title?: string; text?: string; url?: string }
const TTL = 24 * 60 * 60 * 1000;

export async function stashShare(db: Idb, form: FormData): Promise<string> {
  await sweepShares(db, Date.now());
  const item: ShareItem = { id: crypto.randomUUID(), receivedAt: Date.now(), files: [] };
  for (const value of form.getAll('files')) if (typeof value !== 'string') item.files.push(value);
  for (const key of ['title', 'text', 'url'] as const) {
    const value = form.get(key);
    if (typeof value === 'string') item[key] = value;
  }
  await db.put('share_inbox', item.id, item);
  return item.id;
}

export async function takeShare(db: Idb, id: string): Promise<ShareItem | null> {
  const item = await db.get<ShareItem>('share_inbox', id);
  if (!item) return null;
  await db.delete('share_inbox', id);
  return Date.now() - item.receivedAt >= TTL ? null : item;
}

export async function sweepShares(db: Idb, now: number): Promise<number> {
  const expired = (await db.list<ShareItem>('share_inbox')).filter(item => now - item.receivedAt >= TTL);
  await Promise.all(expired.map(item => db.delete('share_inbox', item.id)));
  return expired.length;
}
