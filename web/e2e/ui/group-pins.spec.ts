import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions, type MockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const TARGET = 'Сборка адаптера';

async function boot(page: Page): Promise<{ hub: FakeHub; api: FakeHubApi; ids: MockupSessions }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

function targetId(api: FakeHubApi): string {
  const row = [...api.list.values()].find((item) => item.title === TARGET);
  expect(row, 'целевая сессия в стенде').toBeTruthy();
  return row!.id;
}

test('group_pin_lifts_the_session_inside_its_group_only_and_survives_reload', async ({ page }) => {
  const { api, ids } = await boot(page);
  const id = targetId(api);
  const tablist = page.getByRole('tablist', { name: 'Группы сессий' });
  const scroll = page.locator('[data-list-scroll]');
  const titles = scroll.locator('article .title');
  const groupTab = tablist.getByRole('tab', { name: /canterlot/ });

  await groupTab.click();
  await expect(groupTab).toHaveAttribute('aria-selected', 'true');
  await expect(groupTab).toContainText('3');
  await expect(page.getByRole('button', { name: /^Закреплённые/ })).toHaveCount(0);
  await expect(titles.nth(2)).toHaveText(TARGET);

  // Меню открыто с вкладки группы: закрепляет в ней, а не в «Все».
  await page.getByRole('button', { name: `Действия: ${TARGET}` }).click();
  const pin = page.getByRole('switch', { name: 'Закрепить в группе «canterlot»' });
  await expect(pin).not.toBeChecked();
  await pin.click();
  await expect(page.getByRole('switch', { name: 'Открепить в группе «canterlot»' })).toBeChecked();
  await expect.poll(() => api.list.get(id)?.group_pins.map((item) => item.group_id)).toEqual([ids.canterlot]);
  await page.keyboard.press('Escape');

  // Верх группы занимает «Закреплённые», счёт вкладки не меняется.
  await expect(page.getByRole('button', { name: /^Закреплённые/ })).toBeVisible();
  await expect(titles.first()).toHaveText(TARGET);
  await expect(groupTab).toContainText('3');

  // Во «Все» сессия не закреплена и остаётся в своей группе.
  await tablist.getByRole('tab', { name: /^Все/ }).click();
  await expect(titles.first()).toHaveText('Восстановление потока');
  await expect.poll(() => api.list.get(id)?.pinned).toBe(false);
  const beforeGroup = await titles.allTextContents();
  expect(beforeGroup.indexOf(TARGET)).toBeGreaterThan(1);

  // Перезагрузка сохраняет закрепление и выбранную вкладку.
  await groupTab.click();
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(tablist.getByRole('tab', { name: /canterlot/ })).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('button', { name: /^Закреплённые/ })).toBeVisible();
  await expect(titles.first()).toHaveText(TARGET);

  // Открепление возвращает обычный порядок и тоже переживает перезагрузку.
  await page.getByRole('button', { name: `Действия: ${TARGET}` }).click();
  const unpin = page.getByRole('switch', { name: 'Открепить в группе «canterlot»' });
  await unpin.click();
  await expect(page.getByRole('switch', { name: 'Закрепить в группе «canterlot»' })).not.toBeChecked();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: /^Закреплённые/ })).toHaveCount(0);
  await expect(titles.nth(2)).toHaveText(TARGET);
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(page.getByRole('button', { name: /^Закреплённые/ })).toHaveCount(0);
  await expect(titles.nth(2)).toHaveText(TARGET);
  await expect.poll(() => api.list.get(id)?.group_pins).toEqual([]);
});
