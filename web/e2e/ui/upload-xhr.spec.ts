import { test, expect } from './fixtures';

test('xhr_patch_sender_reports_progress_and_sets_required_headers', async ({ page }) => {
  await page.goto('/__test/primitives');
  await page.waitForFunction(() => typeof window.__canterlotPatchChunk === 'function');

  let interceptedHeaders: Record<string, string> = {};
  let interceptedMethod = '';

  await page.route('**/api/v1/attachments/test-att-1/content', async (route) => {
    const req = route.request();
    interceptedMethod = req.method();
    interceptedHeaders = req.headers();
    await route.fulfill({
      status: 204,
      headers: {
        'Upload-Offset': '2048',
        'X-Server-Time-Ms': '1700000000000'
      }
    });
  });

  const evalResult = await page.evaluate(async () => {
    const patch = window.__canterlotPatchChunk;
    if (!patch) throw new Error('patchChunk is not exposed');
    let progressEvents = 0;
    const result = await patch('/api/v1/attachments/test-att-1/content', new Uint8Array(1024), 1024, {
      onProgress: () => {
        progressEvents += 1;
      }
    });
    return { progressEvents, finalOffset: String(result.offset) };
  });

  expect(interceptedMethod).toBe('PATCH');
  expect(interceptedHeaders['upload-offset']).toBe('1024');
  expect(interceptedHeaders['content-type']).toBe('application/octet-stream');
  expect(interceptedHeaders['x-canterlot-client']).toBe('1');
  expect(interceptedHeaders['x-correlation-id']).toBeTruthy();
  expect(interceptedHeaders['origin']).toBeTruthy();
  expect(interceptedHeaders['idempotency-key']).toBeUndefined();
  expect(evalResult.finalOffset).toBe('2048');
});

test('hashing_runs_in_a_worker_with_progress_and_cancellation', async ({ page }) => {
  await page.goto('/__test/primitives');
  await page.waitForFunction(() => typeof window.__canterlotHash === 'function');
  const hashed = await page.evaluate(async () => {
    const hash = window.__canterlotHash;
    if (!hash) throw new Error('hashInWorker is not exposed');
    const file = new File([new Uint8Array(3000)], 'sample.bin');
    const seen: number[] = [];
    const digest = await hash(file, (done) => seen.push(done), new AbortController().signal);
    return { digest, last: seen.at(-1), size: file.size };
  });
  expect(hashed.digest).toMatch(/^[0-9a-f]{64}$/);
  expect(hashed.last).toBe(hashed.size);

  const cancelled = await page.evaluate(async () => {
    const hash = window.__canterlotHash;
    if (!hash) throw new Error('hashInWorker is not exposed');
    const file = new File([new Uint8Array(2 * 1024 * 1024)], 'large.bin');
    const controller = new AbortController();
    try {
      await hash(
        file,
        () => {
          controller.abort();
        },
        controller.signal
      );
      return 'resolved';
    } catch (err) {
      return err instanceof DOMException ? err.name : 'other';
    }
  });
  expect(cancelled).toBe('AbortError');
});

declare global {
  interface Window {
    __canterlotPatchChunk?: (
      url: string,
      chunk: Blob | Uint8Array,
      offset: number,
      options?: { onProgress?: (loaded: number, total: number) => void }
    ) => Promise<{ offset: number }>;
    __canterlotHash?: (blob: Blob, onProgress: (done: number) => void, signal: AbortSignal) => Promise<string>;
  }
}
