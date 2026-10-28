// @vitest-environment node
import 'fake-indexeddb/auto';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { UploadManager } from '$lib/uploads/manager';
import type { AttachmentRecord, CreatedAttachment } from '$lib/uploads/uploader';
import { openDb, purgeAllStores } from '$lib/storage/idb';
import { ApiError } from '$lib/api/http';

describe('uploads/manager', () => {
  const ownerId = 'manager-test-owner';
  let manager: UploadManager;

  beforeEach(async () => {
    vi.restoreAllMocks();
    await purgeAllStores();
    const db = await openDb(ownerId);
    manager = new UploadManager({ db });
  });

  it('uploads_reject_empty_oversize_and_the_eleventh_file_with_russian_reasons', async () => {
    // 1. Empty file (size === 0)
    const emptyFile = new File([], 'empty.txt');
    expect(() => manager.add('c-1', emptyFile, 0)).toThrow('Файл пустой');

    // 2. Oversize file (> 104857600 bytes)
    const oversizeFile = new File(['x'], 'big.bin');
    Object.defineProperty(oversizeFile, 'size', { value: 104857601 });
    expect(() => manager.add('c-1', oversizeFile, 0)).toThrow('Файл больше 100 МиБ');

    // 3. 11th file (currentDraftCount + 1 > 10)
    const normalFile = new File(['valid content'], 'file.txt');
    expect(() => manager.add('c-1', normalFile, 10)).toThrow('Не более 10 файлов в одном сообщении');
  });

  it('slice_size_is_the_upload_chunk_bytes_from_config', async () => {
    const file = new File([new Uint8Array(20 * 1024 * 1024)], 'chunk-test.bin'); // 20 MB
    const chunkBytes = 4 * 1024 * 1024; // 4 MB chunk from server config

    const patchCalls: number[] = [];
    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: 'att-chunk',
        state: 'uploading',
        upload: { offset: 0, upload_chunk_bytes: chunkBytes }
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: file.size }),
      patchContent: vi.fn().mockImplementation(async (_id: string, chunk: Blob, offset: number) => {
        patchCalls.push(chunk.size);
        return { offset: offset + chunk.size };
      }),
      completeAttachment: vi.fn().mockResolvedValue({ id: 'att-chunk', state: 'ready' }),
      deleteAttachment: vi.fn()
    };

    const uploading = new UploadManager({ db: await openDb(ownerId), api: mockApi });
    const upload = uploading.add('c-1', file, 0);
    await vi.waitFor(() => expect(upload.phase).toBe('ready'), { timeout: 15000 });

    // All chunks except the last should equal chunkBytes (4MB)
    for (let i = 0; i < patchCalls.length - 1; i++) {
      expect(patchCalls[i]).toBe(chunkBytes);
    }
  });

  it('upload_state_survives_reload_and_resumes_from_stored_blob', async () => {
    const db = await openDb(ownerId);
    const file = new File(['stored file contents'], 'resume.txt');
    const hanging = {
      createAttachment: vi.fn().mockResolvedValue({
        id: 'att-stored',
        state: 'uploading',
        upload: { offset: 0, upload_chunk_bytes: 1024 }
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: file.size }),
      patchContent: vi.fn(() => new Promise<{ offset: number }>(() => undefined)),
      completeAttachment: vi.fn(),
      deleteAttachment: vi.fn()
    };
    const first = new UploadManager({ db, api: hanging });
    first.add('c-stored', file, 0);
    await vi.waitFor(async () => {
      const rows = await db.list<{ attachmentId?: string; file?: File; createKey?: string; offset?: number }>('uploads');
      expect(rows.some((row) => row.attachmentId === 'att-stored' && row.file instanceof File && row.createKey)).toBe(true);
    });

    const mockApi = {
      createAttachment: vi.fn(),
      headContent: vi.fn().mockResolvedValue({ offset: 5, length: file.size }),
      patchContent: vi.fn().mockResolvedValue({ offset: file.size }),
      completeAttachment: vi.fn().mockResolvedValue({ id: 'att-stored', state: 'ready' }),
      deleteAttachment: vi.fn()
    };
    const reloadedManager = new UploadManager({ db, api: mockApi });
    await reloadedManager.resumePending();
    expect(mockApi.createAttachment).not.toHaveBeenCalled();
    expect(mockApi.headContent).toHaveBeenCalledWith('att-stored');
    expect(mockApi.patchContent).toHaveBeenCalled();
  });

  it('concurrency_is_limited_to_two_and_429_backs_off', async () => {
    let activeUploads = 0;
    let maxActiveUploads = 0;
    let attempts429 = 0;

    const mockApi = {
      createAttachment: vi.fn().mockImplementation(async () => {
        activeUploads++;
        if (activeUploads > maxActiveUploads) maxActiveUploads = activeUploads;
        await new Promise(r => setTimeout(r, 20));
        activeUploads--;
        return {
          id: `att-${Math.random()}`,
          state: 'uploading',
          upload: { offset: 0, upload_chunk_bytes: 1024 * 1024 }
        };
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 100 }),
      patchContent: vi.fn().mockImplementation(async () => {
        attempts429++;
        if (attempts429 === 1) {
          throw new ApiError(429, 'too_many_requests', 'Лимит запросов', 'hub_db', 'safe_same_key', 'c1');
        }
        return { offset: 100 };
      }),
      completeAttachment: vi.fn().mockResolvedValue({ id: 'att-ok', state: 'ready' }),
      deleteAttachment: vi.fn()
    };

    const uploading = new UploadManager({ db: await openDb(ownerId), api: mockApi });

    const files = [
      new File(['a'], '1.txt'),
      new File(['b'], '2.txt'),
      new File(['c'], '3.txt'),
      new File(['d'], '4.txt')
    ];

    const uploads = files.map((file) => uploading.add('c-1', file, 0));
    await vi.waitFor(() => expect(uploads.every((item) => item.phase === 'ready')).toBe(true), { timeout: 15000 });
    // Concurrency limit is 2
    expect(maxActiveUploads).toBeLessThanOrEqual(2);
    // 429 was backed off and retried successfully
    expect(attempts429).toBeGreaterThan(1);
  });

  it('cancel_deletes_remote_and_local_state', async () => {
    const db = await openDb(ownerId);
    const mockDelete = vi.fn().mockResolvedValue(undefined);
    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: 'att-to-cancel',
        state: 'uploading',
        upload: { offset: 0, upload_chunk_bytes: 1024 * 1024 }
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 100 }),
      patchContent: vi.fn().mockImplementation(() => new Promise(() => {})), // hangs until cancelled
      completeAttachment: vi.fn(),
      deleteAttachment: mockDelete
    };

    const uploading = new UploadManager({ db, api: mockApi });

    const file = new File(['cancel me'], 'cancel.txt');
    const upload = uploading.add('c-1', file, 0);
    await vi.waitFor(() => expect(upload.attachmentId).toBe('att-to-cancel'));

    await uploading.cancel(upload.id);

    await vi.waitFor(() => expect(upload.phase).toBe('cancelled'));
    expect(mockDelete).toHaveBeenCalledWith('att-to-cancel');
    expect(await db.get('uploads', upload.id)).toBeUndefined();
  });

  it('retry_rereads_the_same_record_after_a_failure', async () => {
    const db = await openDb(ownerId);
    const file = new File(['retry me'], 'retry.txt');
    let patches = 0;
    const api = {
      createAttachment: vi.fn().mockResolvedValue({
        id: 'att-retry',
        state: 'uploading',
        upload: { offset: 0, upload_chunk_bytes: 1024 }
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: file.size }),
      patchContent: vi.fn(async () => {
        patches += 1;
        if (patches === 1) throw new ApiError(400, 'invalid_request', 'Плохой кусок', 'browser', 'never', 'c');
        return { offset: file.size };
      }),
      completeAttachment: vi.fn().mockResolvedValue({ id: 'att-retry', state: 'ready' }),
      deleteAttachment: vi.fn()
    };
    const uploading = new UploadManager({ db, api });
    const upload = uploading.add('c-1', file, 0);
    await vi.waitFor(() => expect(upload.phase).toBe('failed'));
    const kept = await db.list<{ file?: File; createKey?: string; attachmentId?: string }>('uploads');
    expect(kept).toHaveLength(1);
    expect(kept[0]?.file).toBeInstanceOf(File);
    expect(kept[0]?.createKey).toBeTruthy();
    expect(kept[0]?.attachmentId).toBe('att-retry');
    uploading.retry(upload.id);
    await vi.waitFor(() => expect(upload.phase).toBe('ready'));
    expect(patches).toBe(2);
  });

  it('recovery_stays_within_concurrency_and_keeps_the_blob', async () => {
    const db = await openDb(ownerId);
    const files = [new File(['a'], 'a.txt'), new File(['b'], 'b.txt'), new File(['c'], 'c.txt')];
    const seeding = {
      createAttachment: vi.fn(async () => ({
        id: `att-${crypto.randomUUID()}`,
        state: 'uploading',
        upload: { offset: 0, upload_chunk_bytes: 1024 }
      })),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 1 }),
      patchContent: vi.fn(() => new Promise<{ offset: number }>(() => undefined)),
      completeAttachment: vi.fn(),
      deleteAttachment: vi.fn()
    };
    const first = new UploadManager({ db, api: seeding });
    for (const file of files) first.add('c-1', file, 0);
    await vi.waitFor(async () => {
      const rows = await db.list<{ file?: File }>('uploads');
      expect(rows.filter((row) => row.file instanceof File)).toHaveLength(3);
    });
    let live = 0;
    let maxLive = 0;
    const api = {
      createAttachment: vi.fn(),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 1 }),
      patchContent: vi.fn(async () => {
        live += 1;
        maxLive = Math.max(maxLive, live);
        await new Promise((resolve) => setTimeout(resolve, 30));
        live -= 1;
        return { offset: 1 };
      }),
      completeAttachment: vi.fn().mockResolvedValue({ id: 'att', state: 'ready' }),
      deleteAttachment: vi.fn()
    };
    const second = new UploadManager({ db, api });
    await second.resumePending();
    expect(maxLive).toBeLessThanOrEqual(2);
    expect(maxLive).toBeGreaterThan(0);
  });

  it('limits_accept_the_exact_maximum_and_the_tenth_file', () => {
    const dbOpen = openDb(ownerId);
    return dbOpen.then((db) => {
      const hasher = { sha256: vi.fn(async () => 'abc') };
      const limited = new UploadManager({
        db,
        hasher,
        api: {
          createAttachment: vi.fn(() => new Promise<CreatedAttachment>(() => undefined)),
          headContent: vi.fn(() => new Promise<{ offset: number; length: number }>(() => undefined)),
          patchContent: vi.fn(() => new Promise<{ offset: number }>(() => undefined)),
          completeAttachment: vi.fn(() => new Promise<AttachmentRecord>(() => undefined))
        },
        limits: () => ({ maxBytes: 104857600, maxPerInput: 10 })
      });
      const edge = new File([new Uint8Array(1)], 'edge.bin');
      Object.defineProperty(edge, 'size', { value: 104857600 });
      expect(() => limited.add('c-1', edge, 9)).not.toThrow();
      expect(() => limited.add('c-1', edge, 10)).toThrow('Не более 10 файлов в одном сообщении');
      expect(() => limited.add('c-1', new File([], 'empty.bin'), 0)).toThrow('Файл пустой');
      expect(hasher.sha256).not.toHaveBeenCalled();
    });
  });

  it('configured_slice_and_byte_progress_reach_the_view', async () => {
    const db = await openDb(ownerId);
    const file = new File([new Uint8Array(2500)], 'progress.bin');
    const sizes: number[] = [];
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const api = {
      createAttachment: vi.fn().mockResolvedValue({
        id: 'att-progress',
        state: 'uploading',
        upload: { offset: 0, upload_chunk_bytes: 1000 }
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: file.size }),
      patchContent: vi.fn(async (_id: string, chunk: Blob, offset: number) => {
        sizes.push(chunk.size);
        if (sizes.length === 1) await gate;
        return { offset: offset + chunk.size };
      }),
      completeAttachment: vi.fn().mockResolvedValue({ id: 'att-progress', state: 'ready' }),
      deleteAttachment: vi.fn()
    };
    const uploading = new UploadManager({ db, api, sliceBytes: 4096 });
    const upload = uploading.add('c-1', file, 0);
    await vi.waitFor(() => expect(upload.phase).toBe('uploading'));
    expect(upload.sent).toBe(0);
    release?.();
    await vi.waitFor(() => expect(upload.phase).toBe('ready'));
    expect(upload.sent).toBe(file.size);
    expect(sizes.slice(0, -1).every((size) => size === 1000)).toBe(true);
  });
});
