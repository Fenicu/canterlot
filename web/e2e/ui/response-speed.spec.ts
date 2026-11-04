import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup04 } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

test('turn_sheet_shows_generation_speed_and_first_token', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [{ id: 'claude', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['high'] }], permission_modes: ['owner', 'bypass'] }]
  });
  const { id, runId } = seedMockup04(api);
  Object.assign(api.runs.get(runId)!, {
    speed: [{ model: 'opus-5.5', output_tokens: 1842, generation_ms: 12972, tokens_per_second: 142, first_token_ms: 1800 }]
  });
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
  const sheet = page.getByRole('dialog', { name: 'Об ответе' });
  const row = sheet.locator('[data-role="run-speed"]');
  await expect(row).toContainText('Скорость');
  await expect(row).toContainText('генерация 142 ток/с · первый токен 1,8 с');
  // Подпись и значение не вылезают за лист и на телефоне.
  const sheetBox = await sheet.boundingBox();
  const rowBox = await row.boundingBox();
  expect(rowBox!.x + rowBox!.width).toBeLessThanOrEqual(sheetBox!.x + sheetBox!.width + 0.5);
  await page.screenshot({ path: `test-results/turn-speed-${test.info().project.name}.png` });
});

test('turn_sheet_without_measured_speed_shows_no_speed_row', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [{ id: 'codex', title: 'Codex', provider: 'codex', models: [{ id: 'gpt', title: 'gpt', efforts: ['high'] }], permission_modes: ['owner'] }]
  });
  const { id, runId } = seedMockup04(api);
  Object.assign(api.runs.get(runId)!, { speed: [] });
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();
  await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
  const sheet = page.getByRole('dialog', { name: 'Об ответе' });
  await expect(sheet.getByText('Длительность')).toBeVisible();
  await expect(sheet.locator('[data-role="run-speed"]')).toHaveCount(0);
});

test('limits_page_lists_median_speed_per_model_for_today_and_the_week', async ({ page }) => {
  await bootSecondary(page, '/limits');
  const table = page.getByRole('table', { name: 'Скорость моделей' });
  await expect(table).toBeVisible();
  const rows = table.locator('tbody tr');
  await expect(rows).toHaveCount(2);
  // Быстрая модель за неделю идёт первой; без ходов сегодня ячейка так и говорит.
  await expect(rows.nth(0)).toContainText('beam');
  await expect(rows.nth(0)).toContainText('OpenCode');
  await expect(rows.nth(0).locator('td').nth(1)).toHaveText('нет ходов');
  await expect(rows.nth(0).locator('td').nth(2)).toContainText('1 240 ток/с');
  await expect(rows.nth(0).locator('td').nth(2)).toContainText('старт 0,4 с');
  await expect(rows.nth(1)).toContainText('Opus 5.5');
  await expect(rows.nth(1).locator('td').nth(1)).toContainText('68 ток/с');
  await expect(rows.nth(1).locator('td').nth(1)).toContainText('старт 2,1 с');
  await expect(rows.nth(1).locator('td').nth(2)).toContainText('71 ток/с');
  await expect(page.getByText(/Codex не передаёт время рассуждений/)).toBeVisible();
  // Сводка стоит рядом с расходом по сессиям, ниже него.
  const usage = await page.getByRole('table', { name: 'Сегодня по сессиям' }).boundingBox();
  const speed = await table.boundingBox();
  expect(speed!.y).toBeGreaterThan(usage!.y);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await table.scrollIntoViewIfNeeded();
  await page.screenshot({ path: `test-results/limits-speed-${test.info().project.name}.png` });
});
