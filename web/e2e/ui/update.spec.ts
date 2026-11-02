import type { Page } from '@playwright/test';
import { test, expect } from './fixtures';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';

async function boot(page: Page) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

/** Имя глобального объекта сборки меняется вместе с версией: по нему видно, что загружен новый JS. */
const buildKey = (page: Page) => page.evaluate(() => Object.keys(window).find(key => key.startsWith('__sveltekit_')) ?? '');

test('version_json_offers_an_update_and_one_press_lands_on_the_new_build', async ({ page, context, baseURL }) => {
  const { hub } = await boot(page);
  const before = await buildKey(page);
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await expect(page.locator('[data-update]')).toBeVisible({ timeout: 10_000 });
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await page.waitForFunction(key => Object.keys(window).some(k => k.startsWith('__sveltekit_') && k !== key), before, { timeout: 20_000 });
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  expect(await buildKey(page)).not.toBe(before);
  const version = await page.evaluate(async () => (await (await fetch('/_app/version.json')).json()).version);
  expect(version).toBe('ui-update-fixture');
  await expect.poll(() => page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length)).toBe(0);
  expect(await page.evaluate(() => caches.keys())).toEqual([]);
});

test('the_update_banner_does_not_appear_without_a_version_change', async ({ page }) => {
  await boot(page);
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await page.waitForTimeout(500);
  await expect(page.locator('[data-update]')).toHaveCount(0);
});
