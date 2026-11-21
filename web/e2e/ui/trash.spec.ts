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
  // Опустевшая корзина прячет свой чип, пока она не выбрана; полоса стола показывает её всегда, без счётчика.
  const railed = (await page.locator('[data-group-rail]').count()) > 0;
  await expect(page.getByRole('tab', { name: railed ? /^Корзина 0$/ : /Корзина/ })).toHaveCount(railed ? 1 : 0);
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

test('a_search_result_disappears_when_the_session_is_trashed_elsewhere', async ({ page }) => {
  const { api, id } = await boot(page);
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('Сессия для корзины');
  await expect(page.locator(`[data-session="${id}"]`)).toBeVisible();
  api.trashConversation(id);
  await expect(page.locator(`[data-session="${id}"]`)).toHaveCount(0);
});

test('trash_tab_loads_its_own_paginated_listing', async ({ page }) => {
  const { api, id } = await boot(page);
  api.trashConversation(id);
  const calls: string[] = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (url.pathname === '/api/v1/conversations' && url.searchParams.get('filter') === 'trashed') {
      calls.push(url.search);
    }
  });
  await page.getByRole('tab', { name: /Корзина/ }).click();
  await expect(page.locator(`[data-session="${id}"]`)).toContainText('удалится через');
  expect(calls.length).toBeGreaterThan(0);
});

declare global {
  interface Window {
    __animated?: { role: string | null; props: string[] }[];
  }
}

async function recordAnimations(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const log: NonNullable<Window['__animated']> = [];
    window.__animated = log;
    const original = Element.prototype.animate;
    Element.prototype.animate = function (this: Element, keyframes: Keyframe[] | PropertyIndexedKeyframes | null, options?: number | KeyframeAnimationOptions) {
      const frames = Array.isArray(keyframes) ? keyframes : [];
      log.push({ role: this.getAttribute('role'), props: [...new Set(frames.flatMap((frame) => Object.keys(frame)))] });
      return original.call(this, keyframes as Keyframe[], options);
    };
  });
}

const toastMoves = (page: Page) => page.evaluate(() => window.__animated!.filter((entry) => entry.role === 'status').map((entry) => entry.props));

test('trash_toast_arrives_and_leaves_with_a_move_and_a_fade', async ({ page }) => {
  await recordAnimations(page);
  const { id } = await boot(page);
  await moveToTrash(page);
  const toast = page.getByRole('status').filter({ hasText: 'Сессия в корзине' });
  await expect(toast).toBeVisible();
  // Появление: сдвиг и прозрачность; само место тоста посередине экрана не меняется.
  await expect.poll(async () => (await toastMoves(page)).length).toBeGreaterThanOrEqual(1);
  expect((await toastMoves(page))[0]).toEqual(expect.arrayContaining(['opacity', 'transform']));
  const box = (await toast.boundingBox())!;
  const view = page.viewportSize()!;
  await page.waitForFunction(() => document.querySelector('[role="status"]')?.getAnimations().length === 0);
  const settled = (await toast.boundingBox())!;
  expect(Math.abs(settled.x + settled.width / 2 - view.width / 2)).toBeLessThanOrEqual(2);
  expect(settled.y).toBeGreaterThanOrEqual(box.y - 40);
  const before = (await toastMoves(page)).length;
  await page.getByRole('button', { name: 'Вернуть' }).click();
  // Уход: тост ещё есть, пока рисуется выход, но уже не принимает нажатий.
  await expect.poll(async () => (await toastMoves(page)).length).toBeGreaterThan(before);
  expect((await toastMoves(page)).at(-1)).toEqual(expect.arrayContaining(['opacity', 'transform']));
  await expect(page.getByText('Сессия в корзине')).toHaveCount(0);
  await expect(page.locator(`[data-session="${id}"]`)).toBeVisible();
});

test('trash_toast_only_fades_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await recordAnimations(page);
  await boot(page);
  await moveToTrash(page);
  await expect(page.getByRole('status').filter({ hasText: 'Сессия в корзине' })).toBeVisible();
  await page.getByRole('button', { name: 'Вернуть' }).click();
  await expect(page.getByText('Сессия в корзине')).toHaveCount(0);
  for (const props of await toastMoves(page)) expect(props).not.toContain('transform');
});
