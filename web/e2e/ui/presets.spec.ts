import type { Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { FakeHub, FakeHubApi, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

async function bootPresets(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  seedPresets(api);
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
  await page.goto('/presets');
  await expect(page.locator('[data-presets-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

test('presets_screen_lists_builtins_first_and_marks_agent_presets', async ({ page }) => {
  await bootPresets(page);
  const items = page.locator('[data-preset]');
  await expect(items.first()).toBeVisible();
  const titles = await items.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-preset')));
  expect(titles[0]).toBe('clean');
  expect(titles[1]).toBe('owner');
  expect(titles).toContain('seed-executor');
  await expect(page.getByText('Без инструкций')).toBeVisible();
  await expect(page.getByText('Как у владельца')).toBeVisible();
  await expect(page.getByText('Исполнитель (английский)')).toBeVisible();
  await expect(page.locator('[data-preset="seed-executor"] [data-agent-mark]')).toHaveCount(0);
  await expect(page.locator('[data-preset="agent-draft"] [data-agent-mark]')).toBeVisible();
  await expect(page.locator('[data-preset="agent-draft"]')).toContainText('Создан агентом');
});

test('create_edit_and_delete_preset_with_confirmation', async ({ page }) => {
  await bootPresets(page);
  await page.getByRole('button', { name: 'Новая инструкция' }).click();
  // Поле текста открывается на несколько строк, а не капсулой в одну.
  expect((await page.getByLabel('Текст').boundingBox())!.height).toBeGreaterThanOrEqual(120);
  await page.getByLabel('Название').fill('Проверочный пресет');
  await page.getByLabel('Текст').fill('Отвечай кратко.');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.locator('[data-preset]').filter({ hasText: 'Проверочный пресет' })).toBeVisible();
  await page.locator('[data-preset]').filter({ hasText: 'Проверочный пресет' }).getByRole('button', { name: 'Изменить' }).click();
  await page.getByLabel('Текст').fill('Отвечай подробно.');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.locator('[data-preset]').filter({ hasText: 'Проверочный пресет' }).getByText('версия 2')).toBeVisible();
  await page.locator('[data-preset]').filter({ hasText: 'Проверочный пресет' }).getByRole('button', { name: 'Удалить' }).click();
  await expect(page.getByText('Сессии, где он применён')).toBeVisible();
  await page.getByRole('button', { name: 'Удалить инструкцию' }).click();
  await expect(page.locator('[data-preset]').filter({ hasText: 'Проверочный пресет' })).toHaveCount(0);
});

test('stale_revision_shows_conflict_and_keeps_the_draft', async ({ page }) => {
  const { api } = await bootPresets(page);
  await page.locator('[data-preset="seed-executor"]').getByRole('button', { name: 'Изменить' }).click();
  await page.getByLabel('Текст').fill('Мой черновик правки.');
  api.bumpPresetRevision('seed-executor', 'Конкурентная правка.');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.getByText(/Инструкции уже изменены/)).toBeVisible();
  await expect(page.getByText('Мой черновик правки.')).toBeVisible();
  await page.getByRole('button', { name: 'Обновить' }).click();
  await expect(page.getByLabel('Текст')).toHaveValue('Конкурентная правка.');
});

test('presets_screen_has_no_axe_violations_at_360px', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await bootPresets(page);
  const results = await new AxeBuilder({ page }).analyze();
  const serious = results.violations
    .filter((violation) => violation.impact === 'serious' || violation.impact === 'critical')
    .map((violation) => `${violation.id} (${violation.impact})`);
  expect(serious).toEqual([]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= 360)).toBe(true);
});
