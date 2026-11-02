import type { BrowserContext, Page } from '@playwright/test';
import { test, expect } from './fixtures';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';

declare global {
  interface Window {
    legacyReady?: { registrations: number; caches: string[]; controller: boolean; scope: string };
    legacyAfter?: { registrations: number; caches: string[] };
    legacyError?: string;
  }
}

const buildKey = (page: Page) => page.evaluate(() => Object.keys(window).find(key => key.startsWith('__sveltekit_')) ?? '');

async function bootOldWorker(page: Page, context: BrowserContext, baseURL: string) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await context.addCookies([{ name: 'pwa-legacy', value: '1', url: baseURL }]);
  await page.goto('/__fixtures__/legacy-sw.html');
  await page.waitForFunction(() => Boolean(window.legacyReady || window.legacyError), undefined, { timeout: 15_000 });
  expect(await page.evaluate(() => window.legacyError)).toBeUndefined();
  const ready = await page.evaluate(() => window.legacyReady!);
  expect(ready.controller).toBe(true);
  expect(ready.caches).toContain('canterlot-shell-legacy');
  return { hub, api, ids };
}

async function expectNewBuild(page: Page, hub: FakeHub) {
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length)).toBe(0);
  expect(await page.evaluate(() => caches.keys())).toEqual([]);
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  const html = await (await page.request.get('/sessions')).text();
  const expected = html.match(/__sveltekit_[a-z0-9]+/)?.[0];
  expect(expected).toBeTruthy();
  expect(await buildKey(page)).toBe(expected);
}

test('loading_the_new_build_unregisters_the_old_worker_and_deletes_its_caches', async ({ page, context, baseURL }) => {
  const { hub } = await bootOldWorker(page, context, baseURL!);
  await context.clearCookies({ name: 'pwa-legacy' });
  await page.goto('/sessions');
  await expectNewBuild(page, hub);
});

test('the_kill_switch_unregisters_navigates_and_leaves_no_caches', async ({ page, context, baseURL }) => {
  const { hub } = await bootOldWorker(page, context, baseURL!);
  await page.evaluate(() => sessionStorage.setItem('legacy-sw-done', '1'));
  await context.clearCookies({ name: 'pwa-legacy' });
  await page.evaluate(async () => {
    const registration = await navigator.serviceWorker.getRegistration();
    await registration?.update();
  });
  await page.waitForFunction(() => Boolean(window.legacyAfter), undefined, { timeout: 20_000 });
  expect(await page.evaluate(() => window.legacyAfter)).toEqual({ registrations: 0, caches: [] });
  await page.goto('/sessions');
  await expectNewBuild(page, hub);
});
