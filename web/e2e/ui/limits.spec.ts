import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const SHOTS = process.env.LIMITS_SHOTS_DIR;

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  const theme = await page.locator('html').getAttribute('data-theme');
  await page.screenshot({ path: join(SHOTS, `${name}-${test.info().project.name}-${theme ?? 'dark'}.png`), fullPage: false });
}

test('limits_history_follows_mockup_10_with_an_accessible_data_table', async ({ page }) => {
  const { historyRequests } = await bootSecondary(page, '/limits');
  const main = page.locator('main');
  await expect(main.getByRole('img', { name: /Дневные снимки лимитов/ })).toBeVisible();
  await expect(main.locator('[data-series="five"]').first()).toHaveCSS('height', /.+/);
  await expect(main.locator('[data-series="weekly"]')).toHaveCount(14);
  const table = main.getByRole('table', { name: 'Дневные снимки лимитов' });
  await expect(table).toHaveCount(1);
  await expect(table.getByRole('row')).toHaveCount(15);
  await expect(main.getByText('Claude сейчас')).toBeVisible();
  await expect(main.getByText('Codex сейчас')).toBeVisible();
  await expect(main.getByText('Субагент · haiku').first()).toBeVisible();
  await expect(main.getByText('приблизительно', { exact: true }).first()).toBeVisible();
  await expect(main.getByText(/Кэш включён во входные токены/)).toBeVisible();
  expect(historyRequests).toHaveLength(2);
  for (const query of historyRequests) { expect(query.get('tz')).toBeTruthy(); expect(query.get('from')).toMatch(/^\d{4}-\d{2}-\d{2}T/); expect(query.get('to')).toMatch(/^\d{4}-\d{2}-\d{2}T/); }
});

test('subtitle_names_the_shown_providers_and_cards_keep_their_shape', async ({ page }) => {
  const phone = test.info().project.name === 'phone';
  if (phone) await page.setViewportSize({ width: 384, height: 900 });
  await bootSecondary(page, '/limits', null, (api) => {
    const claude = api.limits.find((item) => item.provider === 'claude')!;
    claude.weekly!.used_permille = 790;
    claude.five_hour = null;
    claude.five_hour_absent = true;
    const observed = new Date(Date.now() - 60_000).toISOString();
    const reset = new Date(Date.now() + 3_600_000).toISOString();
    api.limits.push({
      provider: 'opencode',
      weekly_reserve_active: false,
      five_hour: { used_permille: 125, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 340, resets_at: reset, observed_at: observed },
      monthly: { used_permille: 93, resets_at: reset, observed_at: observed }
    });
  });
  await expect(page.locator('header.topbar p')).toHaveText('Claude, Codex и OpenCode');
  const cards = page.locator('main .current section');
  await expect(cards).toHaveCount(3);
  await expect(cards.nth(0).locator('.value')).toHaveText('21% / нед');
  await expect(cards.nth(2)).toContainText('мес · 91%');
  // Одинаковая структура: крупное значение с окном в одну строку, сброс — отдельной.
  for (let index = 0; index < 3; index += 1) {
    const card = cards.nth(index);
    const value = card.locator('strong');
    const reset = card.locator('small');
    await expect(value).toHaveCSS('white-space', 'nowrap');
    await expect(reset).toHaveCSS('display', 'block');
    const valueBox = await value.boundingBox();
    const resetBox = await reset.boundingBox();
    expect(resetBox!.y).toBeGreaterThanOrEqual(valueBox!.y + valueBox!.height - 1);
  }
  // На ширине телефона карточки встают друг под друга, на настольной — в один ряд.
  const boxes = await Promise.all([0, 1, 2].map((index) => cards.nth(index).boundingBox()));
  if (phone) {
    expect(boxes[1]!.y).toBeGreaterThanOrEqual(boxes[0]!.y + boxes[0]!.height);
    expect(boxes[2]!.y).toBeGreaterThanOrEqual(boxes[1]!.y + boxes[1]!.height);
  } else {
    expect(Math.abs(boxes[1]!.y - boxes[0]!.y)).toBeLessThanOrEqual(1);
    expect(boxes[1]!.x).toBeGreaterThan(boxes[0]!.x);
    expect(boxes[2]!.x).toBeGreaterThan(boxes[1]!.x);
  }
  expect(await cards.nth(0).innerText()).not.toMatch(/недсброс|5чсброс/);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'limits-cards');
  await page.locator('main').getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'limits-cards');
});
