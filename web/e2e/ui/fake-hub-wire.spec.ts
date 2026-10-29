import { expect, test } from './fixtures';
import { FakeHub } from '../../testing/fake-hub';
import type { ClientOp, ServerOp } from '../../src/lib/sync/wire';

declare global {
  interface Window {
    smoke: { frames: ServerOp[]; closes: number[]; connect(): Promise<string>; send(op: ClientOp): void; close(): void };
  }
}
const scope = 'conversation:00000000-0000-4000-8000-000000000020';
test('fake_hub_playwright_wire_serves_ws_and_chaos_in_both_browsers', async ({ page }) => {
  const hub = new FakeHub(); await hub.wire.playwright(page);
  hub.setSnapshot(scope, () => null);
  hub.routes.set('/api/v1/config', async route => { await route.fulfill({ json: { ws: { path: '/api/v1/ws', protocol: 'canterlot.v1', delta_window_ms: 25 } } }); });
  await page.goto('/__fixtures__/ws-smoke.html');
  expect(await page.evaluate(() => window.smoke.connect())).toBe('canterlot.v1');
  expect(await page.evaluate(async () => (await (await fetch('/api/v1/config')).json()).ws.protocol)).toBe('canterlot.v1');
  await page.evaluate(scope => window.smoke.send({ op: 'subscribe', id: 's1', scope, after: '0' }), scope);
  await expect.poll(() => page.evaluate(() => window.smoke.frames.some(f => f.op === 'subscribed'))).toBe(true);
  hub.append(scope, { kind: 'journal_closed', payload: { secret: 'internal' } }); hub.flush();
  await expect.poll(() => page.evaluate(() => window.smoke.frames.filter(f => f.op === 'event').length)).toBe(1);
  expect(await page.evaluate(() => window.smoke.frames.find(f => f.op === 'event'))).toMatchObject({ event: { kind: 'hidden', payload: {}, scope_seq: '1' } });
  hub.chaos.cut(); await expect.poll(() => page.evaluate(() => window.smoke.closes.length)).toBe(1);
  hub.append(scope, { kind: 'journal_closed', payload: {} }); hub.flush();
  await page.evaluate(() => window.smoke.connect());
  await page.evaluate(scope => window.smoke.send({ op: 'subscribe', id: 's2', scope, after: '1' }), scope);
  await expect.poll(() => page.evaluate(() => window.smoke.frames.filter(f => f.op === 'event').length)).toBe(2);
  expect(await page.evaluate(() => window.smoke.frames.filter(f => f.op === 'event').at(-1))).toMatchObject({ event: { scope_seq_from: '2', scope_seq: '2' } });
  hub.chaos.close4401(); await expect.poll(() => page.evaluate(() => window.smoke.closes.at(-1))).toBe(4401);
  await page.evaluate(() => window.smoke.connect()); hub.chaos.blackhole(true);
  await page.evaluate(() => window.smoke.send({ op: 'ping', t: 1 })); await page.waitForTimeout(100);
  expect(await page.evaluate(() => window.smoke.frames.some(f => f.op === 'pong' && f.t === 1))).toBe(false);
  hub.chaos.blackhole(false); await page.evaluate(() => window.smoke.send({ op: 'ping', t: 2 }));
  await expect.poll(() => page.evaluate(() => window.smoke.frames.some(f => f.op === 'pong' && f.t === 2))).toBe(true);
  const range = await page.evaluate(async scope => (await (await fetch(`/api/v1/events?scope=${scope}&after=0&limit=256`)).json()), scope);
  expect(range).toMatchObject({ head: '2', events: [{ kind: 'hidden', scope_seq_from: '1', scope_seq: '2' }] });
  const ahead = await page.evaluate(async scope => (await (await fetch(`/api/v1/events?scope=${scope}&after=3&limit=1`)).json()), scope);
  expect(ahead).toMatchObject({ resync_required: { reason: 'cursor_ahead', oldest_available_seq: '1' } });
  await page.evaluate(() => window.smoke.close());
});
