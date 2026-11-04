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

test('limits_page_puts_current_first_and_draws_one_weekly_chart_per_provider', async ({ page }) => {
  const { historyRequests } = await bootSecondary(page, '/limits');
  const main = page.locator('main');
  // Порядок: сейчас, история, расход по сессиям, скорость.
  await expect(main.getByRole('heading', { level: 2 })).toHaveText(['Сейчас', 'История лимитов', 'Сегодня по сессиям', 'Скорость моделей']);
  await expect(main.getByRole('region', { name: 'Claude, текущие лимиты' })).toBeVisible();
  await expect(main.getByRole('region', { name: 'Codex, текущие лимиты' })).toBeVisible();
  // Отдельный график на провайдера с недельными снимками: пятичасовых столбиков нет.
  const charts = main.getByRole('img', { name: /^Недельный остаток по дням\./ });
  await expect(charts).toHaveCount(3);
  await expect(charts.first()).toHaveAccessibleName(/Claude: от \d+ до \d+%, сегодня \d+%/);
  await expect(main.locator('[data-series="five"]')).toHaveCount(0);
  await expect(main.locator('[data-series="weekly"]')).toHaveCount(21);
  // Значение читается без наведения: подпись над каждым столбиком.
  await expect(charts.first().locator('.num')).toHaveCount(7);
  await expect(charts.first().locator('.num').last()).toHaveText(/^\d+%$/);
  const table = main.getByRole('table', { name: 'Недельный остаток по дням' });
  await expect(table).toHaveCount(1);
  await expect(table.getByRole('row')).toHaveCount(22);
  await expect(main.getByText('Субагент · haiku').first()).toBeVisible();
  // Приблизительность сказана один раз заголовком «≈» и примечанием, а не в каждой строке.
  await expect(main.getByText('приблизительно', { exact: true })).toHaveCount(0);
  const usage = main.getByRole('table', { name: 'Сегодня по сессиям' });
  // Числа токенов не рвутся посреди разрядов, а ссылка на сессию — полноценная цель нажатия.
  for (const count of await usage.locator('.count').all()) expect(await count.evaluate((node) => node.getClientRects().length)).toBe(1);
  await expect(usage.locator('.count').first()).toHaveText(/^вх\. [\d ]+$/);
  for (const link of await usage.getByRole('link').all()) expect((await link.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  await expect(main.getByText(/Кэш включён во входные токены/)).toBeVisible();
  // Скорость и время до первого токена — каждое число с единицей в одну строку.
  const speed = main.getByRole('table', { name: 'Скорость моделей' });
  await expect(speed.getByText('старт 0,4 с')).toBeVisible();
  for (const value of await speed.locator('.num').all()) expect(await value.evaluate((node) => node.getClientRects().length)).toBe(1);
  expect(historyRequests.map(query => query.get('provider')).sort()).toEqual(['antigravity', 'claude', 'codex', 'opencode']);
  for (const query of historyRequests) { expect(query.get('tz')).toBeTruthy(); expect(query.get('from')).toMatch(/^\d{4}-\d{2}-\d{2}T/); expect(query.get('to')).toMatch(/^\d{4}-\d{2}-\d{2}T/); }
});

test('antigravity_is_one_named_provider_on_the_limits_page', async ({ page }) => {
  await bootSecondary(page, '/limits', null, (api) => {
    const observed = new Date(Date.now() - 60_000).toISOString();
    const reset = new Date(Date.now() + 3_600_000).toISOString();
    api.limits.push({
      provider: 'antigravity',
      weekly_reserve_active: false,
      five_hour: { used_permille: 260, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 410, resets_at: reset, observed_at: observed }
    });
  });
  await expect(page.locator('header.topbar p')).toHaveText('Claude, Codex и Antigravity');
  const card = page.getByRole('region', { name: 'Antigravity, текущие лимиты' });
  await expect(card).toHaveCount(1);
  await expect(card.locator('.row .value')).toHaveText(['осталось 74%', 'осталось 59%']);
  await expect(card.locator('.row .label')).toHaveText(['5 часов', 'неделя']);
  await expect(card.locator('.row .reset').first()).toHaveText(/^сброс \S/);
});

test('estimated_reading_is_called_out_on_the_limits_page', async ({ page }) => {
  await bootSecondary(page, '/limits', null, (api) => {
    const claude = api.limits.find((item) => item.provider === 'claude');
    if (claude) claude.data_confidence = 'estimated';
  });
  const card = page.locator('main .current section').first();
  await expect(card.locator('h3')).toContainText('оценка');
});

test('current_limits_show_only_existing_windows_in_compact_one_line_rows', async ({ page }) => {
  const phone = test.info().project.name === 'phone';
  if (phone) await page.setViewportSize({ width: 390, height: 900 });
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
  // Окна, которого у провайдера нет, на экране нет вовсе.
  await expect(cards.nth(0).locator('.row')).toHaveCount(1);
  await expect(cards.nth(0).locator('.row')).toContainText('неделя');
  await expect(cards.nth(0).locator('.value')).toHaveText('осталось 21%');
  await expect(cards.nth(2).locator('.label')).toHaveText(['5 часов', 'неделя', 'месяц']);
  await expect(cards.nth(2).locator('.value').last()).toHaveText('осталось 91%');
  // Подпись окна, полоска, остаток и сброс — одна строка, без переносов внутри значений.
  for (const row of await page.locator('main .current .row').all()) {
    const parts = await Promise.all(['.label', '.value', '.reset'].map((part) => row.locator(part).boundingBox()));
    for (const box of parts) {
      expect(Math.abs(box!.y + box!.height / 2 - (parts[1]!.y + parts[1]!.height / 2))).toBeLessThanOrEqual(2);
      expect(box!.height).toBeLessThanOrEqual(24);
    }
  }
  // Плотно: на телефоне три провайдера помещаются в первый экран.
  const third = (await cards.nth(2).boundingBox())!;
  if (phone) expect(third.y + third.height).toBeLessThanOrEqual(844);
  const boxes = await Promise.all([0, 1, 2].map((index) => cards.nth(index).boundingBox()));
  if (phone) {
    expect(boxes[1]!.y).toBeGreaterThanOrEqual(boxes[0]!.y + boxes[0]!.height);
    expect(boxes[2]!.y).toBeGreaterThanOrEqual(boxes[1]!.y + boxes[1]!.height);
  } else {
    expect(Math.abs(boxes[1]!.y - boxes[0]!.y)).toBeLessThanOrEqual(1);
    expect(boxes[1]!.x).toBeGreaterThan(boxes[0]!.x);
  }
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'limits-cards');
  await page.locator('main').getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'limits-cards');
});

test('history_error_keeps_the_current_limits_and_names_the_cause', async ({ page }) => {
  await bootSecondary(page, '/limits');
  // Свой ответ истории перекрывает маршрут стенда, поэтому ставится после него и страница перечитывается.
  await page.route('**/api/v1/limits/history?*', (route) => route.fulfill({ status: 503, json: { error: { code: 'db_unavailable', message: 'База данных временно недоступна', scope: 'hub_db', retry: 'safe_same_key', correlation_id: '00000000-0000-4000-8000-0000000000f1' } } }));
  await page.reload();
  const main = page.locator('main');
  await expect(main.getByRole('alert')).toHaveText('История недоступна: База данных временно недоступна');
  await expect(main.getByRole('button', { name: 'Повторить' })).toBeVisible();
  await expect(main.getByRole('region', { name: 'Claude, текущие лимиты' })).toBeVisible();
  await expect(main.getByRole('region', { name: 'Codex, текущие лимиты' })).toBeVisible();
});

test('without_snapshots_the_page_says_so_instead_of_empty_axes', async ({ page }) => {
  await bootSecondary(page, '/limits');
  // Свой ответ истории перекрывает маршрут стенда, поэтому ставится после него и страница перечитывается.
  await page.route('**/api/v1/limits/history?*', (route) => route.fulfill({ json: { series: [], usage: [] } }));
  await page.reload();
  const main = page.locator('main');
  await expect(main.locator('[data-history-empty]')).toHaveText('Снимков лимитов за эти дни ещё нет');
  await expect(main.getByRole('img', { name: /Недельный остаток по дням/ })).toHaveCount(0);
  await expect(main.getByRole('table', { name: 'Сегодня по сессиям' })).toHaveCount(0);
  await expect(main.getByText('Сегодня завершённых запусков ещё нет')).toBeVisible();
});
