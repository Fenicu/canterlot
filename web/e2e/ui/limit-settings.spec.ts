import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const WARN = 'Жёлтым, когда осталось меньше, %';
const DANGER = 'Красным, когда осталось меньше, %';
const RESERVE = 'Резерв владельца: оставить себе, % недели';

async function open(page: import('@playwright/test').Page) {
  const booted = await bootSecondary(page, '/settings');
  const section = page.locator('section[data-limit-settings]');
  await expect(section.getByRole('heading', { name: 'Предупреждения о лимитах' })).toBeVisible({ timeout: 15_000 });
  return { ...booted, section };
}

test('edit_save_and_reload_limit_settings', async ({ page }) => {
  const { api, section } = await open(page);
  await expect(section.getByLabel(WARN, { exact: true })).toHaveValue('20');
  await expect(section.getByLabel(DANGER, { exact: true })).toHaveValue('5');
  await expect(section.getByLabel(RESERVE, { exact: true })).toHaveValue('5');
  await expect(section.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
  await expect(section.getByRole('button', { name: 'Сбросить к значениям по умолчанию' })).toBeDisabled();

  await section.getByLabel(WARN, { exact: true }).fill('40');
  await section.getByLabel(DANGER, { exact: true }).fill('12');
  await section.getByLabel(RESERVE, { exact: true }).fill('0');
  await section.getByRole('button', { name: 'Сохранить' }).click();

  await expect(section.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
  expect(api.limitSettings).toEqual({
    warn_remaining_pct: 40,
    danger_remaining_pct: 12,
    reserve_remaining_pct: 0
  });
  expect(api.limitSettingsKeys).toHaveLength(1);
  await expect(section.getByRole('button', { name: 'Сбросить к значениям по умолчанию' })).toBeEnabled();

  await page.reload();
  const reloaded = page.locator('section[data-limit-settings]');
  await expect(reloaded.getByLabel(WARN, { exact: true })).toHaveValue('40');
  await expect(reloaded.getByLabel(DANGER, { exact: true })).toHaveValue('12');
  await expect(reloaded.getByLabel(RESERVE, { exact: true })).toHaveValue('0');
});

test('reset_returns_limit_settings_to_defaults', async ({ page }) => {
  const { api } = await open(page);
  api.limitSettings = { warn_remaining_pct: 30, danger_remaining_pct: 10, reserve_remaining_pct: 7 };
  await page.reload();
  const reloaded = page.locator('section[data-limit-settings]');
  await expect(reloaded.getByLabel(WARN, { exact: true })).toHaveValue('30');
  await expect(reloaded.getByRole('button', { name: 'Сбросить к значениям по умолчанию' })).toBeEnabled();
  await reloaded.getByRole('button', { name: 'Сбросить к значениям по умолчанию' }).click();

  await expect(reloaded.getByLabel(WARN, { exact: true })).toHaveValue('20');
  await expect(reloaded.getByLabel(DANGER, { exact: true })).toHaveValue('5');
  await expect(reloaded.getByLabel(RESERVE, { exact: true })).toHaveValue('5');
  expect(api.limitSettings).toBeNull();
  await expect(reloaded.getByRole('button', { name: 'Сбросить к значениям по умолчанию' })).toBeDisabled();
  expect(api.limitSettingsKeys).toHaveLength(1);
});

test('invalid_limit_settings_show_inline_messages_and_are_not_sent', async ({ page }) => {
  const { api, section } = await open(page);
  const save = section.getByRole('button', { name: 'Сохранить' });

  await section.getByLabel(WARN, { exact: true }).fill('10');
  await section.getByLabel(DANGER, { exact: true }).fill('30');
  await save.click();
  await expect(section.getByRole('alert')).toHaveText('Красный порог не больше жёлтого');
  expect(api.limitSettingsKeys).toHaveLength(0);

  await section.getByLabel(RESERVE, { exact: true }).fill('100');
  await save.click();
  await expect(section.getByRole('alert').filter({ hasText: 'Введите целое число от 0 до 99' })).toBeVisible();
  expect(api.limitSettingsKeys).toHaveLength(0);

  await section.getByLabel(WARN, { exact: true }).fill('25');
  await section.getByLabel(DANGER, { exact: true }).fill('6');
  await section.getByLabel(RESERVE, { exact: true }).fill('4');
  await save.click();
  await expect(section.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
  expect(api.limitSettingsKeys).toHaveLength(1);
});

test('limit_settings_fit_a_narrow_phone', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  const { section } = await open(page);
  await expect(section.getByRole('button', { name: 'Сохранить' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test('desktop_strip_recolors_without_reload_after_a_threshold_change', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'desktop', 'desktop keeps the session list mounted beside settings');
  const { api } = await bootSecondary(page, '/settings', null, (api) => {
    const claude = api.limits.find((item) => item.provider === 'claude')!;
    claude.weekly = {
      used_permille: 750,
      resets_at: new Date(Date.now() + 86_400_000).toISOString(),
      observed_at: new Date().toISOString()
    };
    claude.weekly_reserve_active = false;
  });
  const strip = page.locator('aside[data-pane="list"] .limits').first();
  await expect(strip).toBeVisible({ timeout: 15_000 });
  // Свёрнутая полоса — плашки: у Claude показана неделя, остаток в ней меньше.
  const week = strip.locator('[data-limit-plaques] .plaque').first().locator('.pct');
  await expect(week).toHaveAttribute('data-tone', 'normal');
  await expect(week).toHaveText(/25%/);

  const section = page.locator('section[data-limit-settings]');
  await expect(section.getByRole('heading', { name: 'Предупреждения о лимитах' })).toBeVisible();
  await section.getByLabel(WARN, { exact: true }).fill('40');
  await section.getByLabel(DANGER, { exact: true }).fill('10');
  await section.getByRole('button', { name: 'Сохранить' }).click();
  await expect(section.getByRole('button', { name: 'Сохранить' })).toBeDisabled();

  // Список сессий не перезагружался: цвет меняет общий реактивный порог.
  await expect(week).toHaveAttribute('data-tone', 'warning');
  await expect(week).toHaveText(/25%/);
  expect(api.limitSettings).toEqual({
    warn_remaining_pct: 40,
    danger_remaining_pct: 10,
    reserve_remaining_pct: 5
  });
});
