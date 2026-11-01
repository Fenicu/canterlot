import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

test('edit_save_reload', async ({ page }) => {
  const { api } = await bootSecondary(page, '/settings');
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
  const { api } = await bootSecondary(page, '/settings');
  api.spawnWhitelist = [{ harness: 'claude', model: 'removed-model' }];
  await page.reload();
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
  await bootSecondary(page, '/settings');
  const section = page.locator('section[data-spawn-whitelist]');
  await expect(section).toBeVisible();
  await section.getByLabel('Агент', { exact: true }).selectOption('claude');
  await section.getByLabel('Модель', { exact: true }).selectOption('opus');
  await section.getByRole('button', { name: 'Добавить' }).click();
  await expect(section.getByRole('alert')).toHaveText('Такая запись уже есть');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});
