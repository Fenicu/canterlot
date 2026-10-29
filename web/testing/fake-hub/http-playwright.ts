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
}
