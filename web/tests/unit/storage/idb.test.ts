// @vitest-environment node
import 'fake-indexeddb/auto';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { openDb, purgeAllStores } from '$lib/storage/idb';

describe('storage/idb', () => {
  beforeEach(async () => {
    await purgeAllStores();
  });

  it('idb_scopes_all_stores_by_owner_and_supports_prefix_scan', async () => {
    const dbOwner1 = await openDb('owner-1');
    const dbOwner2 = await openDb('owner-2');

    // Test drafts store isolation
    await dbOwner1.put('drafts', 'conv-1', { text: 'Hello from owner 1' });
    await dbOwner2.put('drafts', 'conv-1', { text: 'Hello from owner 2' });

    const val1 = await dbOwner1.get('drafts', 'conv-1');
    const val2 = await dbOwner2.get('drafts', 'conv-1');

    expect(val1).toEqual({ text: 'Hello from owner 1' });
    expect(val2).toEqual({ text: 'Hello from owner 2' });

    // Test prefix listing
    await dbOwner1.put('ops', 'conv-1:op-1', { id: 'op-1', payload: 'a' });
    await dbOwner1.put('ops', 'conv-1:op-2', { id: 'op-2', payload: 'b' });
    await dbOwner1.put('ops', 'conv-2:op-3', { id: 'op-3', payload: 'c' });

    const conv1Ops = await dbOwner1.list<{ id: string }>('ops', 'conv-1:');
    expect(conv1Ops).toHaveLength(2);
    expect(conv1Ops.map(o => o.id).sort()).toEqual(['op-1', 'op-2']);

    const allOps = await dbOwner1.list('ops');
    expect(allOps).toHaveLength(3);

    // Owner 2 sees none of owner 1's ops
    const owner2Ops = await dbOwner2.list('ops');
    expect(owner2Ops).toHaveLength(0);

    // Test delete
    await dbOwner1.delete('drafts', 'conv-1');
    expect(await dbOwner1.get('drafts', 'conv-1')).toBeUndefined();
    // Owner 2 still has it
    expect(await dbOwner2.get('drafts', 'conv-1')).toEqual({ text: 'Hello from owner 2' });

    // Test clear
    await dbOwner1.clear('ops');
    expect(await dbOwner1.list('ops')).toHaveLength(0);
  });

  it('stored_numeric_array_comes_back_as_the_same_array', async () => {
    const db = await openDb('owner-arrays');
    const offsets = [0, 5, 10, 15];
    await db.put('uploads', 'row', { offsets, note: 'keep' });
    const stored = await db.get<{ offsets: number[]; note: string }>('uploads', 'row');
    expect(stored?.offsets).toEqual([0, 5, 10, 15]);
    expect(Array.isArray(stored?.offsets)).toBe(true);
    expect(stored?.offsets).not.toBeInstanceOf(Uint8Array);
  });

  it('stored_file_comes_back_as_file_with_name_type_and_bytes', async () => {
    const db = await openDb('owner-files');
    const bytes = new Uint8Array([115, 116, 111, 114, 101, 100]);
    const file = new File([bytes], 'resume.txt', { type: 'text/plain' });
    await db.put('uploads', 'blob', { file });
    const stored = await db.get<{ file: File }>('uploads', 'blob');
    expect(stored?.file).toBeInstanceOf(File);
    expect(stored?.file.name).toBe('resume.txt');
    expect(stored?.file.type).toBe('text/plain');
    expect(new Uint8Array(await stored!.file.arrayBuffer())).toEqual(bytes);
  });

  it('storing_a_large_blob_does_not_read_its_bytes', async () => {
    const db = await openDb('owner-large');
    const blob = new Blob([new Uint8Array(8 * 1024 * 1024)], { type: 'application/octet-stream' });
    const arrayBuffer = vi.spyOn(Blob.prototype, 'arrayBuffer');
    const stream = vi.spyOn(Blob.prototype, 'stream');
    const slice = vi.spyOn(Blob.prototype, 'slice');
    await db.put('uploads', 'big', { blob });
    expect(arrayBuffer).not.toHaveBeenCalled();
    expect(stream).not.toHaveBeenCalled();
    expect(slice).not.toHaveBeenCalled();
    arrayBuffer.mockRestore();
    stream.mockRestore();
    slice.mockRestore();
  });

  it('writes_resolve_only_when_the_transaction_commits', async () => {
    const db = await openDb('owner-commit');
    const original = IDBObjectStore.prototype.put;
    IDBObjectStore.prototype.put = function (this: IDBObjectStore, value: unknown, key?: IDBValidKey) {
      const request = original.call(this, value, key as IDBValidKey);
      request.addEventListener('success', () => {
        this.transaction.abort();
      });
      return request;
    };
    try {
      let resolved = false;
      const writing = db.put('ops', 'k', { n: 1 }).then(() => {
        resolved = true;
      });
      await expect(writing).rejects.toBeTruthy();
      expect(resolved).toBe(false);
    } finally {
      IDBObjectStore.prototype.put = original;
    }
    expect(await db.get('ops', 'k')).toBeUndefined();
  });
});
