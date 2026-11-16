import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupChat } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function bootChat(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedMockupChat(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, id };
}

function last(hub: FakeHub, id: string) {
  return hub.presence.filter((report) => report.conversation_id === id).at(-1);
}

function reports(hub: FakeHub, id: string) {
  return hub.presence.filter((report) => report.conversation_id === id);
}

async function setVisibility(page: Page, state: 'visible' | 'hidden') {
  await page.evaluate((next) => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => next });
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => next === 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
  }, state);
}

test('opening_a_chat_reports_it_visible', async ({ page }) => {
  const { hub, id } = await bootChat(page);
  await expect.poll(() => last(hub, id)?.visible).toBe(true);
});

test('hiding_the_page_reports_not_visible_and_showing_it_again_reports_visible', async ({ page }) => {
  const { hub, id } = await bootChat(page);
  await expect.poll(() => last(hub, id)?.visible).toBe(true);

  await setVisibility(page, 'hidden');
  await expect.poll(() => last(hub, id)?.visible).toBe(false);

  const before = reports(hub, id).length;
  await setVisibility(page, 'visible');
  await expect.poll(() => reports(hub, id).length).toBeGreaterThan(before);
  expect(last(hub, id)?.visible).toBe(true);
});

test('pagehide_reports_not_visible', async ({ page }) => {
  const { hub, id } = await bootChat(page);
  await expect.poll(() => last(hub, id)?.visible).toBe(true);

  await page.evaluate(() => window.dispatchEvent(new Event('pagehide')));
  await expect.poll(() => last(hub, id)?.visible).toBe(false);
});

test('switching_to_the_sessions_list_reports_not_visible', async ({ page }) => {
  const { hub, id } = await bootChat(page);
  await expect.poll(() => last(hub, id)?.visible).toBe(true);

  if (test.info().project.name === 'phone') {
    await page.getByRole('link', { name: /^(Все сессии|Назад, ждут ответа)/ }).click();
  } else {
    await page.goto('/sessions');
  }
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => last(hub, id)?.visible).toBe(false);
});

test('the_visible_chat_repeats_its_report_every_20_seconds', async ({ page }) => {
  await page.clock.install({ time: new Date() });
  const { hub, id } = await bootChat(page);
  await expect.poll(() => last(hub, id)?.visible).toBe(true);
  const before = reports(hub, id).length;

  await page.clock.fastForward(20_000);
  await expect.poll(() => reports(hub, id).length).toBeGreaterThan(before);

  const afterFirst = reports(hub, id).length;
  await page.clock.fastForward(20_000);
  await expect.poll(() => reports(hub, id).length).toBeGreaterThan(afterFirst);
});
