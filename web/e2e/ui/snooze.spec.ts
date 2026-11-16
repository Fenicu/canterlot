import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function bootSessions(page: Page, seed: (api: FakeHubApi) => string) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seed(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function seedOne(api: FakeHubApi, patch: Parameters<typeof seedConversation>[1] = {}) {
  return seedConversation(api, { title: 'Долгая сессия', ...patch });
}

/** Срок откладывания ставится после засева: «создана» такого поля не несёт. */
function snoozedSeed(api: FakeHubApi, inMs: number): string {
  const id = seedOne(api);
  const at = new Date(Date.now() + inMs).toISOString();
  const row = api.list.get(id);
  if (row) row.snoozed_until = at;
  const detail = api.details.get(id);
  if (detail) detail.snoozed_until = at;
  return id;
}

test('the_session_menu_snoozes_for_an_hour_and_the_collapsed_section_takes_the_row', async ({ page }) => {
  const { id } = await bootSessions(page, (api) => seedOne(api));
  const row = page.locator(`[data-session="${id}"]`);
  await expect(row).toBeVisible();

  await row.getByRole('button', { name: 'Действия: Долгая сессия' }).click();
  await page.getByRole('button', { name: 'Отложить', exact: true }).click();
  await page.getByRole('menuitem', { name: '1 час' }).click();

  const heading = page.getByRole('button', { name: 'Отложенные · 1' });
  await expect(heading).toBeVisible();
  await expect(heading).toHaveAttribute('aria-expanded', 'false');
  await expect(row).toHaveCount(0);
  await heading.click();
  await expect(heading).toHaveAttribute('aria-expanded', 'true');
  await expect(row).toBeVisible();
  await expect(row.getByText(/до \d{2}:\d{2}/)).toBeVisible();
});

test('a_snoozed_row_returns_to_its_group_when_the_time_passes_without_a_reload', async ({ page }) => {
  const { id } = await bootSessions(page, (api) => snoozedSeed(api, 2_000));
  await expect(page.getByRole('button', { name: 'Отложенные · 1' })).toBeVisible();
  await expect(page.locator(`[data-session="${id}"]`)).toHaveCount(0);

  await expect(page.locator(`[data-session="${id}"]`)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('button', { name: /^Отложенные/ })).toHaveCount(0);
});

test('the_session_menu_returns_a_snoozed_session', async ({ page }) => {
  const { id } = await bootSessions(page, (api) => snoozedSeed(api, 3_600_000));
  const heading = page.getByRole('button', { name: 'Отложенные · 1' });
  await expect(heading).toBeVisible();
  await heading.click();

  const row = page.locator(`[data-session="${id}"]`);
  await expect(row).toBeVisible();
  await row.getByRole('button', { name: 'Действия: Долгая сессия' }).click();
  await page.getByRole('button', { name: 'Вернуть', exact: true }).click();

  await expect(page.getByRole('button', { name: /^Отложенные/ })).toHaveCount(0);
  await expect(row).toBeVisible();
  await expect(page.getByRole('button', { name: 'Отложить' })).toHaveCount(0);
});

test('a_snoozed_chat_header_says_until_when_and_returns', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = snoozedSeed(api, 3_600_000);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  const line = page.locator('.chat-header').getByText(/Отложена до \d{2}:\d{2}/);
  await expect(line).toBeVisible();
  await page.locator('.chat-header').getByRole('button', { name: 'Вернуть' }).click();
  await expect(line).toHaveCount(0);
});
