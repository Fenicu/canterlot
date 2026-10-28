import { describe, it, expect, vi, beforeEach } from 'vitest';
import { patchChunk } from '$lib/uploads/patch';
import { AuthRequired, NetworkError, session } from '$lib/api/http';

describe('uploads/patch', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it('patch_uses_a_progress_timeout_not_the_total_mutation_timeout', async () => {
    vi.useFakeTimers();

    const slots: {
      progress: ((event: { loaded: number; total: number; lengthComputable: boolean }) => void) | null;
      abort: (() => void) | null;
      load: (() => void) | null;
    } = { progress: null, abort: null, load: null };
    const requestHeaders: Record<string, string> = {};
    let aborted = false;

    // Mock XMLHttpRequest
    const MockXHR = vi.fn().mockImplementation(function () {
      const xhr = {
        open: vi.fn(),
        setRequestHeader: vi.fn((k: string, v: string) => {
          requestHeaders[k.toLowerCase()] = v;
        }),
        send: vi.fn(),
        abort: vi.fn(() => {
          aborted = true;
          slots.abort?.();
        }),
        status: 204,
        getAllResponseHeaders: vi.fn(() => 'upload-offset: 1000\r\n'),
        getResponseHeader: vi.fn((name: string) => {
          if (name.toLowerCase() === 'upload-offset') return '1000';
          return null;
        }),
        upload: {
          addEventListener: vi.fn((event: string, cb: EventListenerOrEventListenerObject) => {
            if (event === 'progress') {
              slots.progress = cb as unknown as (event: {
                loaded: number;
                total: number;
                lengthComputable: boolean;
              }) => void;
            }
          })
        },
        addEventListener: vi.fn((event: string, cb: EventListenerOrEventListenerObject) => {
          if (event === 'load') slots.load = cb as unknown as () => void;
          if (event === 'abort') slots.abort = cb as unknown as () => void;
        })
      };
      return xhr;
    });

    vi.stubGlobal('XMLHttpRequest', MockXHR);

    const chunk = new Uint8Array(1000);
    const seen: number[] = [];
    const patchPromise = patchChunk('/api/v1/attachments/att-1/content', chunk, 0, {
      progressTimeoutMs: 30000,
      onProgress: (loaded) => seen.push(loaded)
    });

    // Check headers set
    expect(requestHeaders['upload-offset']).toBe('0');
    expect(requestHeaders['content-type']).toBe('application/octet-stream');

    // Simulate progress event at 20s (resets the 30s timeout)
    vi.advanceTimersByTime(20000);
    slots.progress?.({ loaded: 500, total: 1000, lengthComputable: true });
    expect(seen).toEqual([500]);

    // Advance another 20s (total elapsed 40s > 30s timeout, but only 20s since last progress)
    vi.advanceTimersByTime(20000);
    expect(aborted).toBe(false);

    // Complete successfully at 45s total
    vi.advanceTimersByTime(5000);
    slots.load?.();

    const res = await patchPromise;
    expect(res.offset).toBe(1000);
    expect(aborted).toBe(false);

    // Now test failure when NO progress arrives for 30s
    aborted = false;
    const failingPromise = patchChunk('/api/v1/attachments/att-1/content', chunk, 0, {
      progressTimeoutMs: 30000
    });

    vi.advanceTimersByTime(30001);
    expect(aborted).toBe(true);

    await expect(failingPromise).rejects.toThrow(/timeout|таймаут/i);

    vi.useRealTimers();
  });

  it('patch_rejects_login_html_redirects_and_a_missing_offset', async () => {
    session.reset?.();

    function install(xhr: Record<string, unknown>): void {
      vi.stubGlobal('XMLHttpRequest', vi.fn(function MockXHR() {
        const listeners = new Map<string, () => void>();
        const request = {
          open: vi.fn(),
          setRequestHeader: vi.fn(),
          send: vi.fn(() => listeners.get('load')?.()),
          abort: vi.fn(),
          upload: { addEventListener: vi.fn() },
          addEventListener: vi.fn((event: string, cb: () => void) => listeners.set(event, cb)),
          getAllResponseHeaders: vi.fn(() => ''),
          getResponseHeader: vi.fn(() => null),
          responseText: '',
          status: 204,
          responseURL: '/api/v1/attachments/att-1/content',
          ...xhr
        };
        return request;
      }));
    }

    install({
      status: 200,
      responseURL: 'https://login.example/session',
      getResponseHeader: vi.fn((name: string) => (name.toLowerCase() === 'content-type' ? 'text/html' : null)),
      responseText: '<html>login</html>'
    });
    await expect(patchChunk('/api/v1/attachments/att-1/content', new Uint8Array([1, 2, 3, 4]), 0)).rejects.toBeInstanceOf(AuthRequired);
    expect(session.state).toBe('expired');

    session.reset?.();
    install({ status: 401, responseText: '<html>login</html>' });
    await expect(patchChunk('/api/v1/attachments/att-1/content', new Uint8Array([1, 2, 3, 4]), 0)).rejects.toBeInstanceOf(AuthRequired);

    session.reset?.();
    install({ status: 204, getResponseHeader: vi.fn(() => null) });
    await expect(patchChunk('/api/v1/attachments/att-1/content', new Uint8Array([1, 2, 3, 4]), 0)).rejects.toBeInstanceOf(NetworkError);
  });
});
