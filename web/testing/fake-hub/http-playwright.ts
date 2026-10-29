import type { Page } from '@playwright/test';
import type { FakeHub } from './hub';
import type { FakeHubApi } from './api';

const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

export async function installChatRoutes(page: Page, hub: FakeHub, api: FakeHubApi, ownerId: string): Promise<void> {
  await hub.wire.playwright(page);
  hub.routes.set('GET /api/v1/me', async (route) => {
    await route.fulfill({
      json: { login: 'owner', owner_id: ownerId },
      headers: { 'X-Server-Time-Ms': String(Date.now()) }
    });
  });
  await page.route('**/api/v1/conversations/**', async (route) => {
    const url = new URL(route.request().url());
    const parts = url.pathname.split('/');
    const id = parts[4] ?? '';
    const kind = parts[5] ?? '';
    const headers = { 'X-Server-Time-Ms': String(Date.now()) };
    if (route.request().method() === 'GET' && kind === 'snapshot') {
      await route.fulfill({ json: hub.snapshot(`conversation:${id}`), headers });
      return;
    }
    if (route.request().method() === 'GET' && kind === 'history') {
      const limit = url.searchParams.get('limit');
      await route.fulfill({
        json: await api.history(id, {
          before: url.searchParams.get('before') ?? undefined,
          after: url.searchParams.get('after') ?? undefined,
          limit: limit ? Number(limit) : undefined
        }),
        headers
      });
      return;
    }
    if (route.request().method() === 'GET' && kind === 'blocks') {
      const blockId = parts[6] ?? '';
      const offset = Number(url.searchParams.get('offset') ?? 0);
      await route.fulfill({ json: await api.block(id, blockId, offset), headers });
      return;
    }
    if (route.request().method() === 'POST' && kind === 'read') {
      await route.fulfill({ status: 204, headers });
      return;
    }
    await route.fallback();
  });
  await page.route('**/api/v1/media/**', async (route) => {
    await route.fulfill({ status: 200, contentType: 'image/png', body: PNG });
  });

  const inputs = new Map<string, string>();
  const scheduled = new Map<string, string>();
  const attachments = new Map<string, { size: number; offset: number; key: string }>();
  const headers = () => ({ 'X-Server-Time-Ms': String(Date.now()), 'cache-control': 'no-store' });
  const failure = (status: number, code: string, message: string, extra?: Record<string, string>) => ({
    status,
    headers: { ...headers(), ...extra },
    json: {
      error: {
        code,
        message,
        scope: 'delivery',
        retry: 'never',
        correlation_id: '00000000-0000-4000-8000-0000000000e1'
      }
    }
  });

  await page.route('**/api/v1/config', async (route) => {
    await route.fulfill({
      headers: headers(),
      json: {
        limits: {
          max_attachment_bytes: 104_857_600,
          max_attachments_per_input: 10,
          max_input_meta_bytes: 65_536,
          max_input_text_bytes: 262_144,
          queue_limit: 100,
          upload_chunk_bytes: 8_388_608
        }
      }
    });
  });

  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const key = route.request().headers()['idempotency-key'] ?? '';
    const prior = key ? inputs.get(key) : undefined;
    const id = prior ?? crypto.randomUUID();
    if (key && !prior) inputs.set(key, id);
    await route.fulfill({ headers: headers(), json: { id, idempotency_key: key } });
  });

  await page.route('**/api/v1/conversations/*/scheduled', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const key = route.request().headers()['idempotency-key'] ?? '';
    const prior = key ? scheduled.get(key) : undefined;
    const id = prior ?? crypto.randomUUID();
    if (key && !prior) scheduled.set(key, id);
    const body = route.request().postDataJSON() as { run_at?: string; content?: unknown };
    await route.fulfill({
      headers: headers(),
      json: {
        id,
        conversation_id: new URL(route.request().url()).pathname.split('/')[4] ?? '',
        run_at: body.run_at ?? new Date().toISOString(),
        content: body.content ?? [],
        attachment_ids: [],
        state: 'pending',
        input_id: null,
        attempts: 0
      }
    });
  });

  await page.route('**/api/v1/conversations/*/interrupt', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    await route.fulfill({ headers: headers(), json: { status: 'sent_to_agent' } });
  });

  await page.route('**/api/v1/inputs/*/cancel', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    await route.fulfill({ headers: headers(), json: 'cancelled' });
  });

  await page.route('**/api/v1/scheduled/**', async (route) => {
    const method = route.request().method();
    if (method !== 'PATCH' && method !== 'DELETE') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    await route.fulfill({
      headers: headers(),
      json: {
        id,
        conversation_id: id,
        run_at: new Date().toISOString(),
        content: [],
        attachment_ids: [],
        state: method === 'DELETE' ? 'cancelled' : 'pending',
        input_id: null,
        attempts: 0
      }
    });
  });

  await page.route('**/api/v1/attachments/**', async (route) => {
    const url = new URL(route.request().url());
    const method = route.request().method();
    const parts = url.pathname.split('/');
    if (method === 'POST' && parts.length === 4) {
      const key = route.request().headers()['idempotency-key'] ?? crypto.randomUUID();
      const existing = [...attachments.entries()].find(([, item]) => item.key === key);
      if (existing) {
        await route.fulfill({
          headers: headers(),
          json: { id: existing[0], state: 'uploading', upload: { upload_chunk_bytes: 8_388_608 } }
        });
        return;
      }
      const body = route.request().postDataJSON() as { size?: number };
      const id = crypto.randomUUID();
      attachments.set(id, { size: body.size ?? 0, offset: 0, key });
      await route.fulfill({
        headers: headers(),
        json: { id, state: 'uploading', upload: { upload_chunk_bytes: 8_388_608 } }
      });
      return;
    }
    const id = parts[4] ?? '';
    const tail = parts[5] ?? '';
    const record = attachments.get(id);
    if (method === 'HEAD' && tail === 'content') {
      await route.fulfill({
        status: 200,
        headers: { ...headers(), 'Upload-Offset': String(record?.offset ?? 0), 'Upload-Length': String(record?.size ?? 0) },
        body: ''
      });
      return;
    }
    if (method === 'PATCH' && tail === 'content') {
      const offset = Number(route.request().headers()['upload-offset'] ?? '0');
      const current = record?.offset ?? 0;
      if (!record || offset !== current) {
        await route.fulfill(failure(409, 'offset_mismatch', 'Смещение не совпадает', { 'Upload-Offset': String(current) }));
        return;
      }
      record.offset += route.request().postDataBuffer()?.length ?? 0;
      await route.fulfill({
        status: 200,
        headers: { ...headers(), 'Upload-Offset': String(record.offset) },
        body: ''
      });
      return;
    }
    if (method === 'POST' && tail === 'complete') {
      await route.fulfill({ headers: headers(), json: { id, state: 'ready' } });
      return;
    }
    if (method === 'DELETE') {
      attachments.delete(id);
      await route.fulfill({ status: 204, headers: headers() });
      return;
    }
    await route.fallback();
  });
}
