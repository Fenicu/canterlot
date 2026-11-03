import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';
import { APP_VERSION } from '../../testing/version';

const SEEN_KEY = 'canterlot.seen-version';

async function seen(page: import('@playwright/test').Page): Promise<string | null> {
  return page.evaluate((key) => localStorage.getItem(key), SEEN_KEY);
}

test('first_visit_keeps_whats_new_closed_and_remembers_the_version', async ({ page }) => {
  await bootSecondary(page, '/settings');

  await expect(page.getByRole('dialog', { name: 'Что нового' })).toHaveCount(0);
  expect(await seen(page)).toBe(APP_VERSION);
});

test('update_shows_new_entries_once_and_not_after_reload', async ({ page }) => {
  await bootSecondary(page, '/settings');
  await page.evaluate(([key]) => localStorage.setItem(key, '0.9.0'), [SEEN_KEY]);
  await page.reload();

  const dialog = page.getByRole('dialog', { name: 'Что нового' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText(APP_VERSION, { exact: true })).toBeVisible();
  await expect(dialog.getByText('Первая пронумерованная версия.')).toBeVisible();
  // Дата как в остальном приложении: «2 ноября», а не «02.11.2026».
  await expect(dialog.locator('.date').first()).toHaveText(/^\d{1,2} [а-яё]+( \d{4})?$/);
  // Лист открылся сам: фокус на заголовке, а не кольцом на «Закрыть».
  await expect(dialog.getByRole('heading', { name: 'Что нового' })).toBeFocused();
  await expect(dialog.getByRole('button', { name: 'Закрыть' })).not.toBeFocused();
  await page.keyboard.press('Tab');
  await expect(dialog.getByRole('button', { name: 'Закрыть' })).toBeFocused();
  await dialog.getByRole('button', { name: 'Понятно', exact: true }).click();
  await expect(dialog).toHaveCount(0);

  await page.reload();
  await expect(page.getByRole('dialog', { name: 'Что нового' })).toHaveCount(0);
  expect(await seen(page)).toBe(APP_VERSION);
});

test('history_page_lists_entries_and_the_installed_version', async ({ page }) => {
  await bootSecondary(page, '/settings');
  await page.locator('main').getByRole('link', { name: 'История изменений', exact: true }).click();

  await expect(page).toHaveURL(/\/changes$/);
  await expect(page.getByRole('heading', { name: 'История изменений', exact: true })).toBeVisible();
  await expect(page.getByText(/Установленная версия/)).toContainText(APP_VERSION);
  await expect(page.getByText('установлена', { exact: true })).toBeVisible();
  await expect(page.getByText('Первая пронумерованная версия.')).toBeVisible();
});

test('settings_show_both_versions_and_flag_a_mismatch_quietly', async ({ page }) => {
  await bootSecondary(page, '/settings', null, undefined, '9.9.9');

  const main = page.locator('main');
  const versions = main.locator('.versions');
  await expect(versions).toHaveText(`Приложение ${APP_VERSION} · хаб 9.9.9`);
  await expect(main.getByText(/Версии приложения и хаба различаются/)).toBeVisible();
});
