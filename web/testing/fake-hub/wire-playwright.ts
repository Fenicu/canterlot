import type { Page } from '@playwright/test';
import type { ClientOp } from '../../src/lib/sync/wire';
import type { FakeHub } from './hub';

export async function playwrightWire(hub: FakeHub, page: Page): Promise<void> {
  await page.routeWebSocket('**/api/v1/dictation', socket => hub.dictation.connect(socket));
  await page.routeWebSocket('**/api/v1/ws', socket => {
    if (!hub.acceptConnections) {
      socket.close({ code: 1011 });
      return;
    }
    const session = hub.attach({
      clock: { now: () => Date.now(), schedule: (fn, ms) => { const id = setTimeout(fn, ms); return () => clearTimeout(id); } },
      message: op => socket.send(JSON.stringify(op)), close: code => socket.close({ code: code === 1006 ? 1011 : code })
    });
    socket.onMessage(data => {
      try { hub.receive(session, JSON.parse(String(data)) as ClientOp); }
      catch { socket.send(JSON.stringify({ op: 'error', id: '', code: 'bad_message' })); }
    });
    socket.onClose(() => hub.detach(session));
  });
  await page.route('**/api/v1/**', async route => {
    const url = new URL(route.request().url());
    const handler = hub.routes.get(`${route.request().method()} ${url.pathname}`) ?? hub.routes.get(url.pathname);
    if (handler) { await handler(route); return; }
    const headers = { 'Cache-Control': 'no-store', 'X-Server-Time-Ms': String(Date.now()) };
    if (url.pathname === '/api/v1/dictation/status') {
      const answer = hub.dictation.statusResponse()!;
      await route.fulfill({ status: answer.status, json: answer.body, headers }); return;
    }
    if (url.pathname === '/api/v1/dictation/terms') {
      let body: unknown = null;
      try { body = route.request().postDataJSON(); } catch { body = null; }
      const answer = hub.dictation.termsResponse(route.request().method(), body);
      await route.fulfill({ status: answer.status, json: answer.body, headers }); return;
    }
    if (url.pathname === '/api/v1/events') {
      const scope = url.searchParams.get('scope') ?? 'conversations', after = url.searchParams.get('after') ?? '0';
      const result = hub.range(scope, after, Number(url.searchParams.get('limit') ?? 256));
      await route.fulfill({ json: 'resync' in result ? { resync_required: {
        reason: BigInt(after) > BigInt(hub.head(scope)) ? 'cursor_ahead' : 'cursor_too_old',
        oldest_available_seq: hub.log(scope)[0]?.scope_seq ?? (BigInt(hub.head(scope)) + 1n).toString()
      } } : result, headers }); return;
    }
    if (url.pathname.startsWith('/api/v1/snapshot/')) {
      await route.fulfill({ json: hub.snapshot(url.pathname.split('/').at(-1)!), headers }); return;
    }
    await route.fulfill({ status: 404, json: { error: { code: 'not_found', message: 'Маршрут не задан' } }, headers });
  });
}
