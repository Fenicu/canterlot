import { describe, it, expect, vi, beforeEach } from 'vitest';
import {
  getAttachmentContent,
  getMedia,
  markRead,
  me,
  patchScheduled,
  postInput,
  setConversationGroups,
  startExecution
} from '$lib/api/client';
import { uploadLimits, loadConfig } from '$lib/api/config';

describe('api/client', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it('coverage_functions_call_the_generated_paths_with_the_client_header', async () => {
    const fetchMock = vi.fn(async () => {
      return new Response(JSON.stringify({ owner_id: 'u', login: 'u', limits: { max_attachment_bytes: 104857600, max_attachments_per_input: 10, upload_chunk_bytes: 8388608 } }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    });
    vi.stubGlobal('fetch', fetchMock);

    await me();
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/v1/me',
      expect.objectContaining({
        method: 'GET',
        credentials: 'same-origin',
        redirect: 'manual'
      })
    );
    const first = fetchMock.mock.calls[0] as [string, RequestInit] | undefined;
    const headers = new Headers(first?.[1].headers);
    expect(headers.get('X-Canterlot-Client')).toBe('1');
    expect(headers.get('X-Correlation-Id')).toBeTruthy();

    fetchMock.mockClear();
    await expect(postInput('c-1', { text: 'hi' } as never, '')).rejects.toThrow();
    expect(fetchMock).not.toHaveBeenCalled();

    await markRead('c-1', { seq: '1' } as never);
    const readCall = fetchMock.mock.calls[0] as [string, RequestInit] | undefined;
    expect(readCall?.[0]).toBe('/api/v1/conversations/c-1/read');
    const readHeaders = new Headers(readCall?.[1].headers);
    expect(readHeaders.get('Idempotency-Key')).toBeNull();
  });

  it('upload_limits_default_until_config_is_loaded', async () => {
    expect(uploadLimits()).toEqual({ maxBytes: 104857600, maxPerInput: 10, sliceBytes: 8388608 });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        new Response(
          JSON.stringify({
            limits: {
              max_attachment_bytes: 100,
              max_attachments_per_input: 2,
              upload_chunk_bytes: 1000,
              max_input_meta_bytes: 1,
              max_input_text_bytes: 1,
              queue_limit: 1
            }
          }),
          { status: 200, headers: { 'content-type': 'application/json' } }
        )
      )
    );
    await loadConfig();
    expect(uploadLimits().maxBytes).toBe(100);
    expect(uploadLimits().maxPerInput).toBe(2);
    expect(uploadLimits().sliceBytes).toBe(1000);
  });

  it('client_exports_cover_the_pilot_table', async () => {
    const urls: string[] = [];
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      urls.push(String(url));
      if (String(url).includes('/media/') || String(url).endsWith('/content')) {
        const type = String(url).includes('/media/') ? 'image/png' : 'application/octet-stream';
        return new Response(new Uint8Array([1, 2, 3]), { status: 200, headers: { 'content-type': type } });
      }
      return new Response(JSON.stringify({ id: 'ok' }), { status: 200, headers: { 'content-type': 'application/json' } });
    }));
    await setConversationGroups('c/1', { group_ids: ['g'] } as never, 'k1');
    await startExecution('c 1', 'k2');
    await patchScheduled('s/1', { text: 'later' } as never, 'k3');
    const media = await getMedia('m/1');
    const file = await getAttachmentContent('a/1');
    expect(urls).toEqual([
      '/api/v1/conversations/c%2F1/groups',
      '/api/v1/conversations/c%201/executions',
      '/api/v1/scheduled/s%2F1',
      '/api/v1/media/m%2F1',
      '/api/v1/attachments/a%2F1/content'
    ]);
    expect(media.data).toBeInstanceOf(ArrayBuffer);
    expect(file.data.byteLength).toBe(3);
  });
});
