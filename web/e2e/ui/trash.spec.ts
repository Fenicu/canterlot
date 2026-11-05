import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const MACHINE = { id: '00000000-0000-4000-8000-0000000000a1', name: 'workstation' };
const HARNESS = { id: 'claude', title: 'Claude Code', provider: 'claude' };
const IDLE = {
  state: 'stopped' as const,
  evidence: {},
  since: '2026-11-05T12:00:00.000Z'
};

async function boot(page: Page): Promise<{ hub: FakeHub; api: FakeHubApi; id: string }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Сессия для корзины',
    machine: MACHINE,
    harness: HARNESS,
    activity: IDLE
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

async function moveToTrash(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Действия: Сессия для корзины' }).click();
  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  await page
    .getByRole('dialog', { name: 'Переместить сессию в корзину?' })
    .getByRole('button', { name: 'В корзину', exact: true })
    .click();
}

test('move_to_trash_shows_the_undo_toast_and_returns_the_session', async ({ page }) => {
  const { api, id } = await boot(page);
  await moveToTrash(page);
  await expect(page.locator(`[data-session="${id}"]`)).toHaveCount(0);
  await expect(page.getByText('Сессия в корзине')).toBeVisible();
  await expect(page.getByRole('tab', { name: /Корзина 1/ })).toBeVisible();
  await page.getByRole('button', { name: 'Вернуть' }).click();
  await expect(page.getByText('Сессия в корзине')).toHaveCount(0);
  await expect(page.locator(`[data-session="${id}"]`)).toBeVisible();
  await expect(page.getByRole('tab', { name: /Корзина 0/ })).toBeVisible();
  expect(api.list.get(id)?.trashed_at).toBeNull();
});

test('trash_tab_lists_the_countdown_and_restores_the_session', async ({ page }) => {
  const { api, id } = await boot(page);
  api.trashConversation(id);
  await expect(page.getByRole('tab', { name: /Корзина 1/ })).toBeVisible();
  await page.getByRole('tab', { name: /Корзина/ }).click();
  await expect(page.getByRole('heading', { name: /Корзина/ })).toBeVisible();
  await expect(page.locator(`[data-session="${id}"]`)).toContainText('удалится через');
  await page.locator(`[data-session="${id}"]`).getByRole('button', { name: 'Восстановить' }).click();
  await expect(page.locator(`[data-session="${id}"]`)).toHaveCount(0);
  await expect(page.getByRole('tab', { name: /Корзина 0/ })).toBeVisible();
  await page.getByRole('tab', { name: /Все/ }).click();
  await expect(page.locator(`[data-session="${id}"]`)).toBeVisible();
});

test('delete_forever_confirms_and_removes_the_session', async ({ page }) => {
  const { api, id } = await boot(page);
  api.trashConversation(id);
  await page.getByRole('tab', { name: /Корзина/ }).click();
  await page
    .locator(`[data-session="${id}"]`)
    .getByRole('button', { name: 'Удалить навсегда' })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Удалить сессию навсегда?' });
  await expect(dialog).toContainText('Это нельзя отменить');
  await dialog.getByRole('button', { name: 'Удалить навсегда' }).click();
  await expect(page.locator(`[data-session="${id}"]`)).toHaveCount(0);
  expect(api.deleted.has(id)).toBe(true);
});

test('a_trashed_chat_offers_restore_instead_of_the_composer', async ({ page }) => {
  const { api, id } = await boot(page);
  api.trashConversation(id);
  await page.goto(`/c/${id}`);
  await expect(page.getByRole('heading', { name: 'Сессия в корзине' })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toHaveCount(0);
  await page.getByRole('button', { name: 'Восстановить' }).click();
  await expect(page.getByRole('heading', { name: 'Сессия в корзине' })).toHaveCount(0);
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  await expect(page.locator('[data-ready]')).toBeVisible();
});

test('a_trashed_session_disappears_from_search', async ({ page }) => {
  const { api, id } = await boot(page);
  api.trashConversation(id);
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('Сессия для корзины');
  await expect(page.getByText('Ничего не найдено')).toBeVisible();
  await expect(page.locator(`[data-session="${id}"]`)).toHaveCount(0);
});
