import type { Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SHOTS = process.env.LIMITS_SHOTS_DIR;

async function boot(page: Page, patch?: (api: FakeHubApi) => void) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  patch?.(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

function claude(api: FakeHubApi) {
  const row = api.limits.find((item) => item.provider === 'claude');
  if (!row) throw new Error('claude limits are seeded');
  return row;
}

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  const theme = await page.locator('html').getAttribute('data-theme');
  const project = test.info().project.name;
  await page.screenshot({ path: join(SHOTS, `${name}-${project}-${theme ?? 'dark'}.png`), fullPage: false });
}

function error(code: string, message: string) {
  return { error: { code, message, scope: 'hub_db', retry: 'after_condition', correlation_id: '00000000-0000-4000-8000-0000000000f1' } };
}

test('collapsed_strip_shows_the_remaining_share_of_both_windows_per_provider', async ({ page }) => {
  await boot(page);
  const strip = page.locator('.limits').first();
  const rows = strip.locator('.row');
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText('Claude');
  await expect(rows.nth(0).locator('.meter').nth(0)).toContainText('5ч');
  await expect(rows.nth(0).locator('.meter').nth(0)).toContainText('66%');
  await expect(rows.nth(0).locator('.meter').nth(1)).toContainText('нед');
  await expect(rows.nth(0).locator('.meter').nth(1)).toContainText('4%');
  await expect(rows.nth(1).locator('.meter').nth(0)).toContainText('91%');
  await expect(rows.nth(1).locator('.meter').nth(1)).toContainText('85%');
  // Полоска показывает остаток: у Claude в пятичасовом окне занято 34%, значит заполнено 66%.
  await expect(rows.nth(0).getByRole('meter', { includeHidden: true }).first()).toHaveAttribute('aria-valuenow', '66');
  await expect(rows.nth(0).locator('.pct[data-tone="danger"]')).toHaveText(/4%/);
  await expect(strip.locator('.reset').first()).toBeHidden();
  const box = await strip.boundingBox();
  const viewport = page.viewportSize();
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport!.width);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-collapsed');
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'strip-collapsed');
});

test('antigravity_is_named_on_the_limits_strip', async ({ page }) => {
  await boot(page, (api) => {
    const observed = new Date(Date.now() - 60_000).toISOString();
    const reset = new Date(Date.now() + 3_600_000).toISOString();
    api.limits.push({
      provider: 'antigravity',
      weekly_reserve_active: false,
      five_hour: { used_permille: 260, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 410, resets_at: reset, observed_at: observed }
    });
  });
  const rows = page.locator('.limits').first().locator('.row');
  await expect(rows).toHaveCount(3);
  const antigravity = rows.filter({ hasText: 'Antigravity' });
  await expect(antigravity).toHaveCount(1);
  await expect(antigravity.locator('.meter').first()).toContainText('74%');
});

test('estimated_reading_is_labelled_in_the_strip_in_words', async ({ page }) => {
  await boot(page, (api) => {
    claude(api).data_confidence = 'estimated';
  });
  const strip = page.locator('.limits').first();
  const rows = strip.locator('.row');
  await expect(rows.nth(0).getByText('оценка', { exact: true })).toBeVisible();
  await expect(rows.nth(1).getByText('оценка')).toHaveCount(0);
});

test('expanded_strip_puts_resets_beside_meters_and_offers_the_refresh_button', async ({ page }) => {
  await boot(page);
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  await expect(strip.locator('.reset').first()).toBeVisible();
  await expect(strip.locator('.reset').first()).toHaveText(/^сброс\s+(\p{L}{2}\s+)?\d\d:\d\d$/u);
  await expect(strip.locator('.reset')).toHaveCount(4);
  await expect(strip.locator('.updated')).toHaveText(/^обновлено \d\d:\d\d$/);
  await expect(strip.getByRole('button', { name: 'Обновить лимиты' })).toBeVisible();
  await expect(strip.getByRole('link', { name: 'История лимитов' })).toBeVisible();
  // Проценты второй раз не повторяются.
  await expect(strip.locator('.open')).not.toContainText('%');
  // Раскрытая полоса — обычная карточка: скругление не режет текст по краям.
  const summaryStyle = await strip.locator('summary').evaluate((node) => {
    const style = getComputedStyle(node);
    return { radius: parseFloat(style.borderTopLeftRadius), paddingLeft: parseFloat(style.paddingLeft), paddingRight: parseFloat(style.paddingRight) };
  });
  expect(summaryStyle.radius).toBeLessThanOrEqual(28);
  expect(summaryStyle.paddingLeft).toBeGreaterThanOrEqual(8);
  expect(summaryStyle.paddingRight).toBeGreaterThanOrEqual(8);
  // Строка «обновлено» начинается там же, где названия провайдеров, без зигзага.
  const updated = (await strip.locator('.updated').boundingBox())!;
  const provider = (await strip.locator('.pname').first().boundingBox())!;
  expect(Math.abs(updated.x - provider.x)).toBeLessThanOrEqual(1);
  await shot(page, 'strip-expanded');
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'strip-expanded');
});

test('refresh_asks_the_hub_shows_pending_and_ends_when_a_new_reading_arrives', async ({ page }) => {
  const { hub, api } = await boot(page);
  let calls = 0;
  await page.route('**/api/v1/limits/refresh', async (route) => {
    calls += 1;
    await route.fulfill({ status: 202, json: { requested: ['claude', 'codex'], retry_after_secs: 15 } });
  });
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  const before = await strip.locator('.updated').textContent();
  await strip.getByRole('button', { name: 'Обновить лимиты' }).click();
  const busy = strip.getByRole('button', { name: 'Обновляю лимиты' });
  await expect(busy).toHaveAttribute('aria-disabled', 'true');
  await shot(page, 'strip-refresh-pending');
  expect(calls).toBe(1);
  const five = claude(api).five_hour!;
  hub.append('conversations', {
    kind: 'limits_changed',
    payload: {
      provider: 'claude',
      window: 'five_hour',
      used_permille: 400,
      resets_at: five.resets_at,
      observed_at: new Date(Date.now() + 120_000).toISOString(),
      weekly_reserve_active: false,
      weekly_reserve_resets_at: null
    }
  });
  hub.flush();
  await expect(strip.getByRole('button', { name: 'Обновить лимиты' })).toBeVisible();
  await expect(strip.locator('.updated')).not.toHaveText(before ?? '');
  await expect(strip.locator('.row').first().locator('.meter').first()).toContainText('60%');
});

test('refresh_refused_by_the_hub_explains_why_without_hanging', async ({ page }) => {
  await boot(page);
  await page.route('**/api/v1/limits/refresh', (route) => route.fulfill({ status: 429, json: error('too_many_requests', 'Слишком много запросов, повторите позже') }));
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  await strip.getByRole('button', { name: 'Обновить лимиты' }).click();
  await expect(strip.getByRole('status')).toContainText('Недавно уже обновляли');
  await expect(strip.getByRole('button', { name: 'Обновить лимиты' })).toBeVisible();
  await page.unroute('**/api/v1/limits/refresh');
  await page.route('**/api/v1/limits/refresh', (route) => route.fulfill({ status: 409, json: error('machine_unavailable', 'Машина недоступна') }));
  await strip.getByRole('button', { name: 'Обновить лимиты' }).click();
  await expect(strip.getByRole('status')).toContainText('Воркер недоступен');
});

test('a_window_past_its_reset_is_dimmed_and_called_stale_in_words', async ({ page }) => {
  await boot(page, (api) => {
    const row = claude(api);
    row.five_hour = { used_permille: 440, resets_at: new Date(Date.now() - 3_600_000).toISOString(), observed_at: new Date(Date.now() - 60_000).toISOString() };
  });
  const strip = page.locator('.limits').first();
  const meter = strip.locator('.row').first().locator('.meter').first();
  await expect(meter).toContainText('устарело');
  await expect(meter).not.toContainText('56%');
  await expect(meter.getByRole('meter', { includeHidden: true })).toHaveCount(1);
  await expect(meter).toHaveAttribute('data-stale', 'true');
  const opacity = await meter.locator('[role="meter"] span').evaluate((node) => getComputedStyle(node).opacity);
  expect(Number(opacity)).toBeLessThan(1);
  await expect(strip.locator('.row').first().locator('.meter').nth(1)).not.toHaveAttribute('data-stale', /.*/);
  await shot(page, 'strip-stale');
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'strip-stale');
});

test('chat_header_strip_is_compact_and_follows_new_readings_while_the_chat_is_open', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  const id = seedConversation(api, { title: 'Лимиты в шапке', harness: { id: 'claude', title: 'Claude', provider: 'claude' } });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const strip = page.locator('.chat-header .limits');
  await expect(strip.locator('.row')).toHaveCount(1);
  await expect(strip.locator('.row')).toContainText('Лимиты');
  await expect(strip.locator('.meter').first()).toContainText('66%');
  const five = claude(api).five_hour!;
  hub.append('conversations', {
    kind: 'limits_changed',
    payload: {
      provider: 'claude',
      window: 'five_hour',
      used_permille: 700,
      resets_at: five.resets_at,
      observed_at: new Date().toISOString(),
      weekly_reserve_active: false,
      weekly_reserve_resets_at: null
    }
  });
  hub.flush();
  await expect(strip.locator('.meter').first()).toContainText('30%');
  await shot(page, 'chat-header-strip');
});

function withoutFiveHour(api: FakeHubApi): void {
  const row = claude(api);
  row.five_hour = null;
  row.five_hour_absent = true;
}

test('a_provider_without_a_five_hour_limit_shows_one_balanced_meter', async ({ page }) => {
  await boot(page, withoutFiveHour);
  const strip = page.locator('.limits').first();
  const claudeRow = strip.locator('.row').nth(0);
  await expect(claudeRow).toContainText('Claude');
  await expect(claudeRow.locator('.meter')).toHaveCount(1);
  await expect(claudeRow.locator('.meter')).toContainText('нед');
  await expect(claudeRow.locator('.meter')).toContainText('4%');
  await expect(claudeRow).not.toContainText('5ч');
  await expect(claudeRow).not.toContainText('устарело');
  // Другой провайдер сохраняет оба счётчика.
  const codexRow = strip.locator('.row').nth(1);
  await expect(codexRow.locator('.meter')).toHaveCount(2);
  // Пустой пятичасовой слот не сдвигает недельное окно: «нед» стоит под «нед».
  const codexWeek = await codexRow.locator('.meter').nth(1).boundingBox();
  const claudeWeek = await claudeRow.locator('.meter').nth(0).boundingBox();
  expect(Math.abs(claudeWeek!.x - codexWeek!.x)).toBeLessThanOrEqual(1);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-one-meter');
  await strip.locator('summary').click();
  await expect(claudeRow.locator('.reset')).toHaveCount(1);
  await shot(page, 'strip-one-meter-open');
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'strip-one-meter-open');
});

test('the_five_hour_meter_disappears_and_returns_with_the_hub_events', async ({ page }) => {
  const { hub, api } = await boot(page);
  const strip = page.locator('.limits').first();
  const claudeRow = strip.locator('.row').nth(0);
  await expect(claudeRow.locator('.meter')).toHaveCount(2);
  const five = claude(api).five_hour!;
  const change = (used: number | null) => hub.append('conversations', {
    kind: 'limits_changed',
    payload: {
      provider: 'claude',
      window: 'five_hour',
      used_permille: used,
      resets_at: used === null ? null : five.resets_at,
      observed_at: new Date().toISOString(),
      weekly_reserve_active: false,
      weekly_reserve_resets_at: null
    }
  });
  change(null);
  hub.flush();
  await expect(claudeRow.locator('.meter')).toHaveCount(1);
  await expect(claudeRow).not.toContainText('5ч');
  change(200);
  hub.flush();
  await expect(claudeRow.locator('.meter')).toHaveCount(2);
  await expect(claudeRow.locator('.meter').first()).toContainText('80%');
});

test('chat_header_and_limits_tab_hide_the_missing_five_hour_window', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  withoutFiveHour(api);
  const id = seedConversation(api, { title: 'Без пятичасового', harness: { id: 'claude', title: 'Claude', provider: 'claude' } });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const header = page.locator('.chat-header .limits');
  await expect(header.locator('.meter')).toHaveCount(1);
  await expect(header).toContainText('нед');
  await expect(header).not.toContainText('5ч');
  await shot(page, 'chat-header-one-meter');

  await installChatRoutes(page, hub, api, ids.pinned);
  await seedSecondaryScreens(page, api, ids, null);
  await page.goto('/limits');
  await expect(page.locator('[data-layout]')).toBeVisible();
  const card = page.getByRole('region', { name: 'Claude, текущие лимиты' });
  await expect(card.locator('.label')).toHaveText(['неделя']);
  await expect(page.getByRole('region', { name: 'Codex, текущие лимиты' }).locator('.label')).toHaveText(['5 часов', 'неделя']);
  await shot(page, 'limits-tab-one-meter');
});

test('opencode_name_fits_beside_the_meters_and_all_meters_stay_aligned', async ({ page }) => {
  await boot(page, (api) => {
    const observed = new Date(Date.now() - 60_000).toISOString();
    const reset = new Date(Date.now() + 3_600_000).toISOString();
    api.limits.push({
      provider: 'opencode',
      weekly_reserve_active: false,
      five_hour: { used_permille: 125, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 340, resets_at: reset, observed_at: observed },
      monthly: { used_permille: 93, resets_at: new Date(Date.now() + 30 * 86_400_000).toISOString(), observed_at: observed }
    });
  });
  const strip = page.locator('.limits').first();
  await expect(strip.locator('.row')).toHaveCount(3);
  const opencode = strip.locator('.row').nth(2);
  await expect(opencode).toContainText('OpenCode');
  await expect(opencode.locator('.meter')).toHaveCount(3);
  await expect(opencode.locator('.meter').nth(2)).toContainText('мес');
  await expect(opencode.locator('.meter').nth(2)).toContainText('91%');
  await expect(opencode.locator('.meter').nth(2).locator('.reset')).toBeHidden();
  const geometry = await strip.evaluate((root) => {
    const rows = [...root.querySelectorAll(':scope .row')];
    const boxes = rows.map((row) => {
      const name = row.querySelector(':scope .name')!.getBoundingClientRect();
      const tag = row.querySelector(':scope .tag')!.getBoundingClientRect();
      return { nameRight: name.right, metersLeft: tag.left };
    });
    const gaps = boxes.map((box) => box.metersLeft - box.nameRight);
    const starts = boxes.map((box) => box.metersLeft);
    return { gaps, starts };
  });
  for (const gap of geometry.gaps) expect(gap).toBeGreaterThanOrEqual(0);
  for (const start of geometry.starts) expect(Math.abs(start - geometry.starts[0]!)).toBeLessThanOrEqual(1);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-opencode');
  await strip.locator('summary').click();
  await expect(opencode.locator('.reset')).toHaveCount(3);
  await expect(opencode.locator('.meter').nth(2).locator('.reset')).toHaveText(/^сброс/);
});

test('monthly_windows_line_up_by_kind_across_providers', async ({ page }) => {
  await boot(page, (api) => {
    const observed = new Date(Date.now() - 60_000).toISOString();
    const reset = new Date(Date.now() + 3_600_000).toISOString();
    const codex = api.limits.find((item) => item.provider === 'codex')!;
    codex.monthly = { used_permille: 300, resets_at: reset, observed_at: observed };
    api.limits.push({
      provider: 'opencode',
      weekly_reserve_active: false,
      five_hour: { used_permille: 125, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 340, resets_at: reset, observed_at: observed },
      monthly: { used_permille: 93, resets_at: reset, observed_at: observed }
    });
  });
  const strip = page.locator('.limits').first();
  const months = strip.locator('.meter[data-window="month"]');
  await expect(months).toHaveCount(2);
  const boxes = await months.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().x));
  expect(Math.abs(boxes[0]! - boxes[1]!)).toBeLessThanOrEqual(1);
  await expect(months.nth(0)).toContainText('мес');
  await expect(months.nth(1)).toContainText('мес');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-monthly');
});

test('alternating_provider_rows_are_tinted_and_the_week_values_stay_aligned', async ({ page }) => {
  await boot(page, (api) => {
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
  const strip = page.locator('.limits').first();
  const rows = strip.locator('.row');
  await expect(rows).toHaveCount(3);
  const backgrounds = () => rows.evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).backgroundColor));
  const weekValues = () => strip.locator('.meter[data-window="week"] .pct').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().x));
  const aligned = (positions: number[]) => {
    for (const x of positions) expect(Math.abs(x - positions[0]!)).toBeLessThanOrEqual(1);
  };

  const collapsedBackgrounds = await backgrounds();
  expect(collapsedBackgrounds[0]).not.toBe(collapsedBackgrounds[1]);
  expect(collapsedBackgrounds[0]).toBe(collapsedBackgrounds[2]);
  aligned(await weekValues());

  await strip.locator('summary').click();
  await expect(strip.locator('.reset').first()).toBeVisible();
  const expandedBackgrounds = await backgrounds();
  expect(expandedBackgrounds[0]).not.toBe(expandedBackgrounds[1]);
  expect(expandedBackgrounds[0]).toBe(expandedBackgrounds[2]);
  aligned(await weekValues());
});

const FOUR_PROVIDERS = ['Claude', 'Codex', 'Antigravity', 'OpenCode'];

/** Ровно те провайдеры, что были у владельца: у Claude только неделя, у OpenCode месяц и все окна устарели. */
function fourProviders(api: FakeHubApi): void {
  const observed = new Date(Date.now() - 60_000).toISOString();
  const reset = new Date(Date.now() + 3_600_000).toISOString();
  const later = new Date(Date.now() + 2 * 86_400_000).toISOString();
  const staleObserved = new Date(Date.now() - 4 * 3_600_000).toISOString();
  const staleReset = new Date(Date.now() - 60_000).toISOString();
  api.limits = [
    {
      provider: 'claude',
      weekly_reserve_active: false,
      five_hour: null,
      five_hour_absent: true,
      weekly: { used_permille: 300, resets_at: later, observed_at: observed }
    },
    {
      provider: 'codex',
      weekly_reserve_active: false,
      five_hour: { used_permille: 100, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 430, resets_at: later, observed_at: observed }
    },
    {
      provider: 'antigravity',
      weekly_reserve_active: false,
      five_hour: { used_permille: 0, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 0, resets_at: later, observed_at: observed }
    },
    {
      provider: 'opencode',
      weekly_reserve_active: false,
      five_hour: { used_permille: 100, resets_at: staleReset, observed_at: staleObserved },
      weekly: { used_permille: 300, resets_at: staleReset, observed_at: staleObserved },
      monthly: { used_permille: 200, resets_at: staleReset, observed_at: staleObserved }
    }
  ];
}

interface WindowMetrics {
  window: string;
  band: string;
  gaugeX: number;
  bandTop: number;
  bandBottom: number;
  meterTop: number;
  meterBottom: number;
  value: string;
  valueOverflow: number;
  resetOverflow: number;
  resetLines: number;
  resetVisible: boolean;
  resetHeight: number;
  resetLineHeight: number;
}

async function windowMetrics(page: Page): Promise<WindowMetrics[]> {
  return page.locator('.limits').first().evaluate((root) => {
    return [...root.querySelectorAll<HTMLElement>('.meter')].map((meter) => {
      const band = meter.closest<HTMLElement>('.row')!;
      const bar = meter.querySelector<HTMLElement>('.bar')!;
      const value = meter.querySelector<HTMLElement>('.pct')!;
      const reset = meter.querySelector<HTMLElement>('.reset');
      const bandBox = band.getBoundingClientRect();
      const meterBox = meter.getBoundingClientRect();
      const resetBox = reset?.getBoundingClientRect();
      return {
        window: meter.dataset.window ?? '',
        band: (band.textContent ?? '').replace(/\s+/g, ' ').trim(),
        gaugeX: bar.getBoundingClientRect().x,
        bandTop: bandBox.top,
        bandBottom: bandBox.bottom,
        meterTop: meterBox.top,
        meterBottom: meterBox.bottom,
        value: (value.textContent ?? '').replace(/^осталось\s*/, '').trim(),
        valueOverflow: value.scrollWidth - value.clientWidth,
        resetOverflow: reset ? reset.scrollWidth - reset.clientWidth : 0,
        resetLines: reset ? reset.getClientRects().length : 0,
        resetVisible: reset ? getComputedStyle(reset).display !== 'none' : false,
        resetHeight: resetBox?.height ?? 0,
        resetLineHeight: reset ? Number.parseFloat(getComputedStyle(reset).lineHeight) : 0
      };
    });
  });
}

async function expectFourProviderStrip(page: Page, open: boolean): Promise<void> {
  const strip = page.locator('.limits').first();
  const isOpen = await strip.evaluate((node) => (node as HTMLDetailsElement).open);
  if (isOpen !== open) await strip.locator('summary').click();
  await expect(strip.locator('.row')).toHaveCount(4);

  const metrics = await windowMetrics(page);
  expect(metrics.map((item) => item.window)).toEqual(['week', 'five', 'week', 'five', 'week', 'five', 'week', 'month']);
  // Одна колонка полосы для всех окон всех провайдеров, в обоих состояниях.
  for (const item of metrics) expect(Math.abs(item.gaugeX - metrics[0]!.gaugeX)).toBeLessThanOrEqual(1);
  // Название провайдера не режется, пока в колонке есть место.
  for (const name of FOUR_PROVIDERS) {
    const label = strip.locator('.meter .name').filter({ hasText: name });
    await expect(label).toHaveCount(1);
    const clipped = await label.evaluate((node) => node.scrollWidth - node.clientWidth);
    expect(clipped, name).toBeLessThanOrEqual(0);
  }
  // Значение устаревшего окна не вылезает из своей колонки, не наезжает на соседа и остаётся читаемым.
  for (const item of metrics) {
    expect(item.valueOverflow, `${item.window}: ${item.value}`).toBeLessThanOrEqual(0);
  }
  const stale = metrics.filter((item) => item.value === 'устарело');
  expect(stale).toHaveLength(3);
  for (const item of stale) expect(item.band).toContain('OpenCode');
  const month = metrics.find((item) => item.window === 'month')!;
  expect(month.band).toContain('OpenCode');
  expect(month.meterTop).toBeGreaterThanOrEqual(month.bandTop - 0.5);
  expect(month.meterBottom).toBeLessThanOrEqual(month.bandBottom + 0.5);
  const summary = await strip.locator('summary').boundingBox();
  expect(summary!.height).toBeGreaterThanOrEqual(44);

  if (open) {
    const resets = metrics.filter((item) => item.resetVisible);
    expect(resets).toHaveLength(5);
    for (const item of resets) {
      expect(item.resetLines, `${item.window} reset lines`).toBe(1);
      expect(item.resetOverflow, `${item.window} reset overflow`).toBeLessThanOrEqual(0);
      expect(Math.abs(item.resetHeight - item.resetLineHeight), `${item.window} reset height`).toBeLessThanOrEqual(1);
    }
  } else {
    expect(metrics.some((item) => item.resetVisible)).toBe(false);
  }
}

test('four_providers_keep_one_aligned_grid_at_the_desktop_sidebar_and_a_390_phone', async ({ page }) => {
  await boot(page, fourProviders);
  for (const width of [2560, 1440, 390]) {
    await page.setViewportSize({ width, height: 900 });
    await expectFourProviderStrip(page, false);
    await shot(page, `strip-four-${width}-collapsed`);
    await expectFourProviderStrip(page, true);
    await shot(page, `strip-four-${width}-expanded`);
    await expectFourProviderStrip(page, false);
    await page.getByRole('button', { name: 'Сменить тему' }).click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
    await expectFourProviderStrip(page, false);
    await shot(page, `strip-four-${width}-collapsed`);
    await expectFourProviderStrip(page, true);
    await shot(page, `strip-four-${width}-expanded`);
    await page.getByRole('button', { name: 'Сменить тему' }).click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    await expectFourProviderStrip(page, false);
  }
});

test('expanded_strip_has_one_shared_grid_across_providers', async ({ page }) => {
  await boot(page, (api) => {
    const observed = new Date(Date.now() - 60_000).toISOString();
    const reset = new Date(Date.now() + 3_600_000).toISOString();
    const codex = api.limits.find((item) => item.provider === 'codex')!;
    codex.monthly = { used_permille: 300, resets_at: reset, observed_at: observed };
    api.limits.push({
      provider: 'opencode',
      weekly_reserve_active: false,
      five_hour: { used_permille: 125, resets_at: reset, observed_at: observed },
      weekly: { used_permille: 340, resets_at: reset, observed_at: observed },
      monthly: { used_permille: 93, resets_at: reset, observed_at: observed }
    });
  });
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  const bars = strip.locator('.bar');
  const barBoxes = await bars.evaluateAll((nodes) => nodes.map((n) => n.getBoundingClientRect().x));
  for (const x of barBoxes) expect(Math.abs(x - barBoxes[0]!)).toBeLessThanOrEqual(1);
  const resets = strip.locator('.reset');
  const resetBoxes = await resets.evaluateAll((nodes) => nodes.map((n) => n.getBoundingClientRect().x));
  for (const x of resetBoxes) expect(Math.abs(x - resetBoxes[0]!)).toBeLessThanOrEqual(1);
  await expect(strip.locator('.pname').filter({ hasText: /.+/ })).toHaveCount(3);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-expanded-grid');
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await shot(page, 'strip-expanded-grid');
});
