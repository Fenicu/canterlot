import type { Page } from '@playwright/test';
import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

// Раздел делегирования живёт в «Лимитах и делегировании»: ждём его заголовка перед нажатиями.
async function settled(page: Page) {
  await expect(page.locator('section[data-spawn-whitelist]').getByRole('heading', { name: 'Делегирование' })).toBeVisible({ timeout: 15_000 });
}

async function openSettings(page: Page) {
  const booted = await bootSecondary(page, '/settings?c=limits');
  await settled(page);
  return booted;
}

test('sync_line_keeps_its_place_so_the_sign_in_section_does_not_move', async ({ page }) => {
  // Часы стоят до первого тика: времени синхронизации ещё нет, но строка уже стоит на месте.
  await page.clock.install();
  await bootSecondary(page, '/settings?c=security');
  await expect(page.locator('main[data-secondary-ready]')).toBeVisible();
  const section = page.locator('[data-sign-in-settings]');
  await expect(section).toBeVisible();
  const value = page.locator('dl.facts dt', { hasText: 'Последняя синхронизация' }).locator('xpath=following-sibling::dd[1]');
  const before = (await section.boundingBox())!.y;
  for (let tick = 0; tick < 30 && (await value.textContent()) === 'ещё не было'; tick += 1) {
    await page.clock.fastForward(1000);
    await page.waitForTimeout(25);
  }
  await expect(value).toHaveText(/^\d{1,2}:\d{2}$/);
  const after = (await section.boundingBox())!.y;
  expect(Math.abs(after - before)).toBeLessThanOrEqual(0.5);
});

test('edit_save_reload', async ({ page }) => {
  const { api } = await openSettings(page);
  const section = page.locator('section[data-spawn-whitelist]');
  await expect(section.getByRole('heading', { name: 'Делегирование' })).toBeVisible();
  await expect(section.getByText('Сессии, которые агент запускает без вопроса')).toBeVisible();
  await expect(section.getByText('Остальное агент запрашивает карточкой')).toBeVisible();
  await expect(section.getByRole('list').getByRole('listitem')).toHaveCount(2);
  await section.getByRole('button', { name: /^Убрать .*opus-5\.5/ }).click();
  await expect(section.getByRole('list').getByRole('listitem')).toHaveCount(1);
  await section.getByLabel('Агент', { exact: true }).selectOption('claude');
  await section.getByLabel('Модель', { exact: true }).selectOption('opus');
  await section.getByRole('button', { name: 'Добавить' }).click();
  await expect(section.getByRole('list').getByRole('listitem')).toHaveCount(2);
  await section.getByRole('button', { name: 'Сохранить' }).click();
  await expect(section.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
  expect(api.spawnWhitelist).toEqual([
    { harness: 'claude', model: 'default' },
    { harness: 'claude', model: 'opus' }
  ]);
  expect(api.spawnWhitelistKeys).toHaveLength(1);
  await page.reload();
  await expect(page.locator('section[data-spawn-whitelist]').getByRole('list').getByRole('listitem')).toHaveCount(2);
  await expect(page.locator('section[data-spawn-whitelist]').getByRole('button', { name: 'Сбросить к списку по умолчанию' })).toBeEnabled();
});

test('reset_to_default', async ({ page }) => {
  const { api } = await openSettings(page);
  api.spawnWhitelist = [{ harness: 'claude', model: 'removed-model' }];
  await page.reload();
  await settled(page);
  const section = page.locator('section[data-spawn-whitelist]');
  await expect(section.getByText('Нет в каталоге машины')).toBeVisible();
  await section.getByRole('button', { name: 'Сбросить к списку по умолчанию' }).click();
  await expect(section.getByRole('list').getByRole('listitem')).toHaveCount(2);
  await expect(section.getByText('Нет в каталоге машины')).toHaveCount(0);
  await expect(section.getByRole('button', { name: 'Сбросить к списку по умолчанию' })).toBeDisabled();
  expect(api.spawnWhitelist).toBeNull();
  expect(api.spawnWhitelistKeys).toHaveLength(1);
  await page.reload();
  await expect(page.locator('section[data-spawn-whitelist]').getByRole('list').getByRole('listitem')).toHaveCount(2);
});

test('whitelist_screen_fits_360px_and_refuses_duplicates', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  await openSettings(page);
  const section = page.locator('section[data-spawn-whitelist]');
  await expect(section).toBeVisible();
  await section.getByLabel('Агент', { exact: true }).selectOption('claude');
  await section.getByLabel('Модель', { exact: true }).selectOption('opus');
  await section.getByRole('button', { name: 'Добавить' }).click();
  await expect(section.getByRole('alert')).toHaveText('Такая запись уже есть');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});
