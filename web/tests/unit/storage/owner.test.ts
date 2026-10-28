import 'fake-indexeddb/auto';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { bindOwner } from '$lib/storage/owner';
import { openDb, purgeAllStores } from '$lib/storage/idb';
import { DraftStore } from '$lib/drafts/store';

describe('storage/owner', () => {
  beforeEach(async () => {
    localStorage.clear();
    await purgeAllStores();
  });

  it('bind_owner_purges_storage_and_local_storage_drafts_on_owner_change', async () => {
    // 1. First binding
    const res1 = await bindOwner('alice');
    expect(res1).toBe('first');

    // Populate data for alice
    const db = await openDb('alice');
    await db.put('drafts', 'conv-1', { text: 'Draft for alice' });
    await db.put('ops', 'op-1', { type: 'send' });
    localStorage.setItem('canterlot:draft:alice:conv-1', JSON.stringify({ text: 'Draft for alice' }));
    localStorage.setItem('canterlot:other_key', 'keep_me');

    // Mock caches if available or stub it
    const mockCacheDelete = vi.fn().mockResolvedValue(true);
    const mockCacheKeys = vi.fn().mockResolvedValue(['cache-v1', 'images']);
    vi.stubGlobal('caches', {
      keys: mockCacheKeys,
      delete: mockCacheDelete
    });

    // 2. Same owner binding
    const res2 = await bindOwner('alice');
    expect(res2).toBe('same');
    expect(await db.get('drafts', 'conv-1')).toEqual({ text: 'Draft for alice' });
    expect(mockCacheDelete).not.toHaveBeenCalled();

    // 3. Changed owner binding
    const res3 = await bindOwner('bob');
    expect(res3).toBe('changed');

    // Check caches were deleted
    expect(mockCacheDelete).toHaveBeenCalledWith('cache-v1');
    expect(mockCacheDelete).toHaveBeenCalledWith('images');

    // Check localStorage draft keys purged, other keys kept
    expect(localStorage.getItem('canterlot:draft:alice:conv-1')).toBeNull();
    expect(localStorage.getItem('canterlot:other_key')).toBe('keep_me');

    // Check IndexedDB stores were cleared
    const dbBob = await openDb('bob');
    expect(await dbBob.get('drafts', 'conv-1')).toBeUndefined();
    // Also alice's previous data in db was cleared
    const dbAliceAgain = await openDb('alice');
    expect(await dbAliceAgain.get('drafts', 'conv-1')).toBeUndefined();
  });

  it('owner_change_purges_all_private_data_of_the_previous_owner', async () => {
    await bindOwner({ owner_id: 'alice' });
    const db = await openDb('alice');
    await db.put('drafts', 'conv-1', { text: 'secret' });
    localStorage.setItem('canterlot.theme', 'dark');
    localStorage.setItem('canterlot.enter', 'newline');
    localStorage.setItem('canterlot.draft-tail.conv-1', '{"text":"secret"}');

    const changed = await bindOwner({ owner_id: 'bob' });
    expect(changed).toBe('changed');
    expect(localStorage.getItem('canterlot.theme')).toBe('dark');
    expect(localStorage.getItem('canterlot.enter')).toBe('newline');
    expect(localStorage.getItem('canterlot.draft-tail.conv-1')).toBeNull();
    expect(await (await openDb('alice')).get('drafts', 'conv-1')).toBeUndefined();
  });

  it('first_bind_keeps_existing_anonymous_prefs_only', async () => {
    localStorage.setItem('canterlot.theme', 'dark');
    localStorage.setItem('canterlot.enter', 'send');
    const first = await bindOwner({ owner_id: 'alice' });
    expect(first).toBe('first');
    expect(localStorage.getItem('canterlot.theme')).toBe('dark');
    expect(localStorage.getItem('canterlot.enter')).toBe('send');
    const again = await bindOwner({ owner_id: 'alice' });
    expect(again).toBe('same');
    expect(localStorage.getItem('canterlot.theme')).toBe('dark');
  });

  it('owner_change_stops_old_writers_and_rejects_a_foreign_mirror', async () => {
    await bindOwner('alice');
    const aliceDb = await openDb('alice');
    const alice = new DraftStore('alice', aliceDb);
    alice.set('c-1', { text: 'alice secret', attachments: [{ id: 'a' }] });
    window.dispatchEvent(new Event('pagehide'));
    expect(JSON.parse(localStorage.getItem('canterlot.draft-tail.c-1')!).ownerId).toBe('alice');

    await bindOwner('bob');
    alice.set('c-1', { text: 'alice secret', attachments: [{ id: 'a' }] });
    window.dispatchEvent(new Event('pagehide'));
    const bobDb = await openDb('bob');
    const bob = new DraftStore('bob', bobDb);
    expect(await bob.load('c-1')).toBeNull();
    await expect(aliceDb.put('drafts', 'c-1', { text: 'resurrected' })).rejects.toThrow(/storage_retired/);
    expect(await bobDb.get('drafts', 'c-1')).toBeUndefined();
    alice.destroy();
    bob.destroy();
  });
});
