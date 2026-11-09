import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect, resizeViewport } from './fixtures';
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
  // Расход сгруппирован по провайдерам: у каждого своя неделя и свой итог за сегодня.
  await expect(usage.locator('tbody .group th')).toHaveText(['Claude', 'Codex', 'Antigravity']);
  await expect(usage.locator('tbody[data-provider="claude"] .group .total')).toHaveText('≈ 3,4%');
  // Провайдер без недельного окна всё равно виден, но вместо выдуманного «≈ 0%» стоит прочерк.
  const antigravity = usage.locator('tbody[data-provider="antigravity"]');
  await expect(antigravity.locator('.group .total')).toHaveText('—');
  await expect(antigravity.locator('tr:not(.group) td:nth-child(3)')).toHaveText(['—', '—']);
  await expect(usage.locator('tbody tr').last().locator('td').nth(2)).toHaveText('—');
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

test('huge_token_counts_stay_inside_their_cells_at_every_width', async ({ page }) => {
  const { ids } = await bootSecondary(page, '/limits');
  // Свой ответ истории перекрывает маршрут стенда: огромные числа не влезают в колонку целиком.
  await page.route('**/api/v1/limits/history?*', (route) => {
    const query = new URL(route.request().url()).searchParams;
    const provider = query.get('provider') ?? 'claude';
    if (provider !== 'claude') return route.fulfill({ json: { series: [], usage: [], speed: [] } });
    const day = new Intl.DateTimeFormat('en-CA', { timeZone: query.get('tz') ?? 'UTC' }).format(new Date());
    const row = (model: string, input_tokens: number, output_tokens: number, weekly_delta_permille: number) => ({
      day, conversation_id: ids.pinned, provider, agent: 'main', model, input_tokens, output_tokens,
      cache_read_tokens: 0, cache_write_tokens: 0, weekly_delta_permille, runs: 1, overlapped_runs: 0
    });
    return route.fulfill({ json: { series: [], usage: [
      row('opus', 1_319_435_536, 4_120_998, 1000),
      row('haiku', 123_456_789_012, 9_876_543_210, 1),
      row('sonnet', 516_733_981, 2_031_449, 0)
    ], speed: [] } });
  });
  await page.reload();
  const table = page.getByRole('table', { name: 'Сегодня по сессиям' });
  await expect(table).toBeVisible();
  const counts = table.locator('.count');
  await expect(counts).toHaveCount(9);
  // Миллионы и миллиарды сокращены, полное число осталось в подсказке.
  await expect(counts.nth(0)).toHaveText('вх. 1,32 млрд');
  await expect(counts.nth(0)).toHaveAttribute('title', 'вх. 1 319 435 536');
  await expect(table.locator('.sr-only').nth(0)).toHaveText('вх. 1 319 435 536');
  await expect(counts.nth(1)).toHaveText('вых. 4,12 млн');
  await expect(counts.nth(2)).toHaveText('≈ 100,0%');
  await expect(counts.nth(3)).toHaveText('вх. 123,5 млрд');
  await expect(counts.nth(3)).toHaveAttribute('title', 'вх. 123 456 789 012');
  await expect(counts.nth(4)).toHaveText('вых. 9,88 млрд');
  await expect(counts.nth(5)).toHaveText('≈ 0,1%');
  await expect(counts.nth(6)).toHaveText('вх. 516,7 млн');
  await expect(counts.nth(6)).toHaveAttribute('title', 'вх. 516 733 981');
  await expect(counts.nth(7)).toHaveText('вых. 2,03 млн');
  await expect(counts.nth(8)).toHaveText('≈ 0,0%');
  for (const index of [0, 1, 3, 4, 6, 7]) await expect(counts.nth(index)).toHaveAttribute('title', /^(вх|вых)\. [\d ]+$/);
  const widths = test.info().project.name === 'phone' ? [320, 360, 390] : [1440];
  for (const width of widths) {
    await resizeViewport(page, { width, height: 844 });
    const fits = await counts.evaluateAll((nodes) => nodes.map((node) => {
      const cell = node.closest('td');
      if (!cell) return null;
      const box = node.getBoundingClientRect();
      const td = cell.getBoundingClientRect();
      const next = cell.nextElementSibling;
      return {
        left: box.left - td.left,
        right: td.right - box.right,
        overlap: next ? box.right - next.getBoundingClientRect().left : -1,
        scroll: node.scrollWidth - node.clientWidth
      };
    }));
    for (const fit of fits) {
      expect(fit).not.toBeNull();
      expect(fit!.left).toBeGreaterThanOrEqual(-0.5);
      expect(fit!.right).toBeGreaterThanOrEqual(-0.5);
      expect(fit!.overlap).toBeLessThanOrEqual(0.5);
      expect(fit!.scroll).toBeLessThanOrEqual(1);
    }
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
    if (SHOTS && test.info().project.name === 'phone') {
      await table.scrollIntoViewIfNeeded();
      await shot(page, `limits-huge-tokens-${width}`);
    }
  }
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
  await expect(card.locator('.status')).toContainText('оценка');
});

test('current_limits_show_only_existing_windows_with_the_text_on_one_line_over_the_gauge', async ({ page }) => {
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
  // Подпись окна, остаток и сброс — одна строка над полосой, без переносов внутри значений.
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

test('current_limits_rows_stay_inside_their_card_at_narrow_widths', async ({ page }) => {
  await bootSecondary(page, '/limits');
  const cards = page.locator('main .current section');
  await expect(cards).toHaveCount(2);
  const widths = test.info().project.name === 'phone' ? [320, 360, 390] : [1440];
  for (const width of widths) {
    await resizeViewport(page, { width, height: 844 });
    const rows = await page.locator('main .current .row').evaluateAll((items) => items.map((row) => {
      const card = row.closest('section')!.getBoundingClientRect();
      return [...row.children].map((cell) => {
        const box = cell.getBoundingClientRect();
        return { text: cell.textContent?.trim() ?? '', left: box.left - card.left, right: card.right - box.right };
      });
    }));
    expect(rows.length).toBeGreaterThan(0);
    for (const cells of rows) {
      for (const cell of cells) {
        expect(cell.left, `${cell.text} at ${width}px`).toBeGreaterThanOrEqual(-0.5);
        expect(cell.right, `${cell.text} at ${width}px`).toBeGreaterThanOrEqual(-0.5);
      }
    }
  }
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
