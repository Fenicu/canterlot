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
  await page.evaluate(async () => { await navigator.serviceWorker.ready; });
  await page.waitForFunction(() => !!navigator.serviceWorker.controller);
  return { hub, api, ids };
}

/** Имя глобального объекта сборки меняется вместе с версией: по нему видно, что загружен новый JS. */
const buildKey = (page: Page) => page.evaluate(() => Object.keys(window).find(key => key.startsWith('__sveltekit_')) ?? '');

async function expectNewBuild(page: Page, hub: FakeHub, before: string) {
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  expect(await buildKey(page)).not.toBe(before);
  const version = await page.evaluate(async () => (await (await fetch('/_app/version.json')).json()).version);
  expect(version).toBe('ui-update-fixture');
}

test('one_press_activates_a_waiting_worker_and_lands_on_the_new_build', async ({ page, context, baseURL }) => {
  const { hub } = await boot(page);
  const before = await buildKey(page);
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  await page.evaluate(async () => { const reg = await navigator.serviceWorker.ready; await reg.update(); });
  await page.waitForFunction(async () => (await navigator.serviceWorker.ready).waiting?.state === 'installed', undefined, { timeout: 20_000 });
  await expect(page.locator('[data-update]')).toBeVisible({ timeout: 10_000 });
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await page.waitForFunction(key => Object.keys(window).some(k => k.startsWith('__sveltekit_') && k !== key), before, { timeout: 20_000 });
  await expectNewBuild(page, hub, before);
});

test('one_press_lands_on_the_new_build_when_only_version_json_knows_about_it', async ({ page, context, baseURL }) => {
  const { hub } = await boot(page);
  const before = await buildKey(page);
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  // SvelteKit проверяет _app/version.json по focus; worker ещё не установлен.
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await expect(page.locator('[data-update]')).toBeVisible({ timeout: 10_000 });
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await page.waitForFunction(key => Object.keys(window).some(k => k.startsWith('__sveltekit_') && k !== key), before, { timeout: 25_000 });
  await expectNewBuild(page, hub, before);
});

test('one_press_while_the_new_worker_is_still_installing_lands_on_the_new_build', async ({ page, context, baseURL }) => {
  const { hub } = await boot(page);
  const before = await buildKey(page);
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  await page.evaluate(async () => { const reg = await navigator.serviceWorker.ready; void reg.update(); });
  await expect(page.locator('[data-update]')).toBeVisible({ timeout: 10_000 });
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await page.waitForFunction(key => Object.keys(window).some(k => k.startsWith('__sveltekit_') && k !== key), before, { timeout: 25_000 });
  await expectNewBuild(page, hub, before);
});

test('an_update_that_cannot_be_checked_says_why_instead_of_staying_silent', async ({ page, context, baseURL }) => {
  await boot(page);
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await expect(page.locator('[data-update]')).toBeVisible({ timeout: 10_000 });
  await page.evaluate(async () => {
    const reg = await navigator.serviceWorker.ready;
    Object.defineProperty(reg, 'update', { configurable: true, value: async () => { throw new Error('offline'); } });
  });
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await expect(page.locator('[data-update]')).toContainText('Не удалось проверить обновление', { timeout: 10_000 });
  await expect(page.getByRole('button', { name: 'Повторить', exact: true })).toBeVisible();
});
