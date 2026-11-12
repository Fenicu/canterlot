import type { Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, resizeViewport, test } from './fixtures';
import type { ProviderStatus } from '../../src/lib/stores/limits';

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

/**
 * Полоса списка в свёрнутом виде — плашки провайдеров; сетка окон, которую проверяют
 * эти тесты, живёт в раскрытой полосе.
 */
async function openStrip(page: Page) {
  const strip = page.locator('.limits').first();
  await expect(strip).toBeVisible();
  if (!(await strip.evaluate((node) => (node as HTMLDetailsElement).open))) await strip.locator('summary').click();
  await expect(strip.locator('.rows')).toBeVisible();
  return strip;
}

/** Тема списка сессий живёт в меню «Ещё»: шапка держит поиск, избранное и меню в одну строку. */
async function setListTheme(page: Page, theme: 'dark' | 'light'): Promise<void> {
  if ((await page.locator('html').getAttribute('data-theme')) === theme) return;
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await page.getByRole('menuitem', { name: theme === 'light' ? 'Светлая тема' : 'Тёмная тема' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
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
  // Свёрнутая полоса: плашка на провайдера с окном, где осталось меньше всего, и отсчётом до его сброса.
  await expectPlaques(page, ['Claude', 'Codex']);
  const plaques = page.locator('[data-limit-plaques] .plaque');
  await expect(plaques.nth(0)).toContainText(/4%\s*нед/);
  await expect(plaques.nth(0).locator('.pct[data-tone="danger"]')).toHaveText(/4%/);
  await expect(plaques.nth(0).locator('.reset-short')).toHaveText(/^(23ч\d+м|1д)$/);
  await expect(plaques.nth(1)).toContainText(/85%\s*нед/);
  const strip = await openStrip(page);
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
  // Свёрнутая полоса сразу говорит, сколько до сброса каждого показанного окна: сжато, без слова «сброс».
  const box = await strip.boundingBox();
  const viewport = page.viewportSize();
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport!.width);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-collapsed');
  await setListTheme(page, 'light');
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
  const rows = (await openStrip(page)).locator('.row');
  await expect(rows).toHaveCount(3);
  const antigravity = rows.filter({ hasText: 'Antigravity' });
  await expect(antigravity).toHaveCount(1);
  await expect(antigravity.locator('.meter').first()).toContainText('74%');
});

test('estimated_reading_is_labelled_in_the_strip_in_words', async ({ page }) => {
  await boot(page, (api) => {
    claude(api).data_confidence = 'estimated';
  });
  const strip = await openStrip(page);
  const rows = strip.locator('.row');
  await expect(rows.nth(0).getByText('оценка', { exact: true })).toBeVisible();
  await expect(rows.nth(1).getByText('оценка')).toHaveCount(0);
});

test('expanded_strip_puts_resets_beside_meters_and_offers_the_refresh_button', async ({ page }) => {
  await boot(page);
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  await expect(strip.locator('.reset').first()).toBeVisible();
  // Раскрытая полоса говорит словами; на узкой полосе слово «сброс» уходит, отсчёт остаётся.
  const words = strip.locator('.reset').first().locator('.reset-full:visible, .reset-bare:visible, .reset-span:visible');
  await expect(words).toHaveText(/^(сброс )?(через )?(59\u00a0мин|1\u00a0ч)$/);
  await expect(strip.locator('.reset-short:visible')).toHaveCount(0);
  await expect(strip.locator('.reset')).toHaveCount(4);
  await expect(strip.locator('.updated')).toHaveText(/^обновлено \d\d:\d\d$/);
  await expect(strip.getByRole('button', { name: 'Обновить лимиты' })).toBeVisible();
  await expect(strip.getByRole('link', { name: 'История лимитов' })).toBeVisible();
  // Проценты второй раз не повторяются.
  await expect(strip.locator('.open')).not.toContainText('%');
  // Раскрытая полоса — обычная карточка: скругление строки не режет текст по краям.
  const rowStyle = await strip.locator('.row').first().evaluate((node) => {
    const style = getComputedStyle(node);
    return { radius: parseFloat(style.borderTopLeftRadius), paddingLeft: parseFloat(style.paddingLeft), paddingRight: parseFloat(style.paddingRight) };
  });
  expect(rowStyle.radius).toBeLessThanOrEqual(28);
  expect(rowStyle.paddingLeft).toBeGreaterThanOrEqual(8);
  expect(rowStyle.paddingRight).toBeGreaterThanOrEqual(8);
  // Строка «обновлено» начинается там же, где названия провайдеров, без зигзага.
  const updated = (await strip.locator('.updated').boundingBox())!;
  const provider = (await strip.locator('.pname').first().boundingBox())!;
  expect(Math.abs(updated.x - provider.x)).toBeLessThanOrEqual(1);
  await shot(page, 'strip-expanded');
  await setListTheme(page, 'light');
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
  // Кнопка уходит в ожидание по нажатию, раньше, чем запрос доходит до маршрута.
  await expect.poll(() => calls).toBe(1);
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
  const strip = await openStrip(page);
  const meter = strip.locator('.row').first().locator('.meter').first();
  await expect(meter).toContainText('устарело');
  await expect(meter).not.toContainText('56%');
  await expect(meter.getByRole('meter', { includeHidden: true })).toHaveCount(1);
  await expect(meter).toHaveAttribute('data-stale', 'true');
  const opacity = await meter.locator('[role="meter"] span').evaluate((node) => getComputedStyle(node).opacity);
  expect(Number(opacity)).toBeLessThan(1);
  await expect(strip.locator('.row').first().locator('.meter').nth(1)).not.toHaveAttribute('data-stale', /.*/);
  await shot(page, 'strip-stale');
  await setListTheme(page, 'light');
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
  const strip = await openStrip(page);
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
  const codexWeek = await codexRow.locator('.meter').nth(1).locator('.tag').boundingBox();
  const claudeWeek = await claudeRow.locator('.meter').nth(0).locator('.tag').boundingBox();
  expect(Math.abs(claudeWeek!.x - codexWeek!.x)).toBeLessThanOrEqual(1);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-one-meter');
  await openStrip(page);
  await expect(claudeRow.locator('.reset')).toHaveCount(1);
  await shot(page, 'strip-one-meter-open');
  await setListTheme(page, 'light');
  await shot(page, 'strip-one-meter-open');
});

test('the_five_hour_meter_disappears_and_returns_with_the_hub_events', async ({ page }) => {
  const { hub, api } = await boot(page);
  const strip = await openStrip(page);
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
  await seedSecondaryScreens(page, api, ids);
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
  const strip = await openStrip(page);
  await expect(strip.locator('.row')).toHaveCount(3);
  const opencode = strip.locator('.row').nth(2);
  await expect(opencode).toContainText('OpenCode');
  await expect(opencode.locator('.meter')).toHaveCount(3);
  await expect(opencode.locator('.meter').nth(2)).toContainText('мес');
  await expect(opencode.locator('.meter').nth(2)).toContainText('91%');
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
  // Свёрнутая полоса показывает месяц, только если в нём осталось меньше всего: у OpenCode это неделя.
  await expect(page.locator('[data-limit-plaques] .plaque').nth(2)).toContainText(/66%\s*нед/);
  const strip = await openStrip(page);
  const months = strip.locator('.meter[data-window="month"]');
  await expect(months).toHaveCount(2);
  await expect(months.first()).toBeVisible();
  const boxes = await months.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().x));
  expect(Math.abs(boxes[0]! - boxes[1]!)).toBeLessThanOrEqual(1);
  await expect(months.nth(0)).toContainText('мес');
  await expect(months.nth(1)).toContainText('мес');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-monthly');
});

test('fable_weekly_window_shows_after_the_weekly_window_and_fits_a_phone', async ({ page }) => {
  await boot(page, (api) => {
    const row = claude(api);
    row.weekly_fable = {
      used_permille: 50,
      resets_at: new Date(Date.now() + 4 * 86_400_000).toISOString(),
      observed_at: new Date(Date.now() - 60_000).toISOString()
    };
  });
  await resizeViewport(page, { width: 320, height: 844 });
  // Квота Fable в плашку не попадает: плашка Claude — общая неделя.
  await expect(page.locator('[data-limit-plaques] .plaque').first()).toContainText(/4%\s*нед/);
  const strip = await openStrip(page);
  const row = strip.locator('.row').first();
  const fable = row.locator('.meter[data-window="fable"]');
  await expect(fable).toHaveCount(1);
  await expect(row.locator('.meter[data-window="week"]')).toContainText('4%');
  await expect(fable).toBeVisible();
  await expect(fable).toContainText('Fable');
  await expect(fable).toContainText('95%');
  await expect(fable.locator('.reset')).toHaveText(/^сброс/);
  const week = row.locator('.meter[data-window="week"]');
  const boxes = await Promise.all([week.boundingBox(), fable.boundingBox()]);
  expect(boxes[1]!.y).toBeGreaterThan(boxes[0]!.y);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-fable');
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
  const strip = await openStrip(page);
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

  await openStrip(page);
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
  visible: boolean;
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
      const tagBox = meter.querySelector<HTMLElement>('.tag')!.getBoundingClientRect();
      const resetBox = reset?.getBoundingClientRect();
      return {
        window: meter.dataset.window ?? '',
        band: (band.textContent ?? '').replace(/\s+/g, ' ').trim(),
        visible: getComputedStyle(meter).display !== 'none' && tagBox.width > 0 && tagBox.height > 0,
        // Без полоски (узкая свёрнутая полоса) колонку окна отмечает значение.
        gaugeX: (bar.getClientRects().length > 0 ? bar : value).getBoundingClientRect().x,
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

/** Каждая колонка полосы совпадает по x у всех строк, где она есть. */
function expectColumnsAligned(items: WindowMetrics[], column: string): void {
  const positions = items.map((item) => item.gaugeX);
  expect(positions.length).toBeGreaterThan(0);
  for (const x of positions) expect(Math.abs(x - positions[0]!), column).toBeLessThanOrEqual(1);
}

/** Свёрнутая полоса списка: плашка на провайдера, все в один ряд и в пределах полосы. */
async function expectPlaques(page: Page, names: readonly string[]): Promise<void> {
  const strip = page.locator('.limits').first();
  const plaques = strip.locator('[data-limit-plaques] .plaque');
  await expect(plaques).toHaveCount(names.length);
  for (const [index, name] of names.entries()) await expect(plaques.nth(index).locator('.pname')).toHaveText(name);
  const boxes = await plaques.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect()).map((box) => ({ top: box.top, left: box.left, right: box.right })));
  const outer = (await strip.boundingBox())!;
  for (const box of boxes) {
    expect(Math.abs(box.top - boxes[0]!.top)).toBeLessThanOrEqual(1);
    expect(box.left).toBeGreaterThanOrEqual(outer.x - 0.5);
    expect(box.right).toBeLessThanOrEqual(outer.x + outer.width + 0.5);
  }
  const summary = await strip.locator('summary').boundingBox();
  expect(summary!.height).toBeGreaterThanOrEqual(44);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
}

async function expectFourProviderStrip(page: Page, open: boolean): Promise<void> {
  const strip = page.locator('.limits').first();
  const isOpen = await strip.evaluate((node) => (node as HTMLDetailsElement).open);
  if (isOpen !== open) await strip.locator('summary').click();
  if (!open) {
    await expectPlaques(page, FOUR_PROVIDERS);
    return;
  }
  await expect(strip.locator('.row')).toHaveCount(4);
  // Раскрытая полоса показывает каждое окно своей строкой.
  const windows = ['week', 'five', 'week', 'five', 'week', 'five', 'week', 'month'];
  await expect.poll(async () => (await windowMetrics(page)).filter((item) => item.visible).map((item) => item.window)).toEqual(windows);

  const all = await windowMetrics(page);
  const metrics = all.filter((item) => item.visible);
  const month = all.find((item) => item.window === 'month');
  // Раскрытая полоса: одна колонка полосы на все окна.
  expectColumnsAligned(metrics, 'expanded');

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
  const summary = await strip.locator('summary').boundingBox();
  expect(summary!.height).toBeGreaterThanOrEqual(44);

  expect(month?.band).toContain('OpenCode');
  expect(month!.meterTop).toBeGreaterThanOrEqual(month!.bandTop - 0.5);
  expect(month!.meterBottom).toBeLessThanOrEqual(month!.bandBottom + 0.5);
  const resets = metrics.filter((item) => item.resetVisible);
  expect(resets).toHaveLength(5);
  for (const item of resets) {
    expect(item.resetLines, `${item.window} reset lines`).toBe(1);
    expect(item.resetOverflow, `${item.window} reset overflow`).toBeLessThanOrEqual(0);
    expect(Math.abs(item.resetHeight - item.resetLineHeight), `${item.window} reset height`).toBeLessThanOrEqual(1);
  }
}

test('four_providers_keep_one_aligned_grid_at_the_desktop_sidebar_and_a_390_phone', async ({ page }) => {
  await boot(page, fourProviders);
  for (const width of [2560, 1440, 390]) {
    await resizeViewport(page, { width, height: 900 });
    await expectFourProviderStrip(page, false);
    await shot(page, `strip-four-${width}-collapsed`);
    await expectFourProviderStrip(page, true);
    await shot(page, `strip-four-${width}-expanded`);
    await expectFourProviderStrip(page, false);
    await setListTheme(page, 'light');
    await expectFourProviderStrip(page, false);
    await shot(page, `strip-four-${width}-collapsed`);
    await expectFourProviderStrip(page, true);
    await shot(page, `strip-four-${width}-expanded`);
    await setListTheme(page, 'dark');
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
  await expect(strip.locator('.reset').first()).toBeVisible();
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
  await setListTheme(page, 'light');
  await shot(page, 'strip-expanded-grid');
});

for (const motion of ['full', 'reduced'] as const) {
  test(`meter_slides_to_a_new_reading_by_transform_and_stands_still_on_first_paint_${motion}`, async ({ page }) => {
    if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
    const { hub, api } = await boot(page);
    // В узкой свёрнутой полосе (боковая колонка рабочего стола) полоски уступают место отсчёту; в раскрытой они всегда есть.
    await page.locator('.limits').first().locator('summary').click();
    const fill = page.locator('.limits').first().locator('.row').nth(0).locator('[role="meter"] span').first();
    await expect(fill).toBeVisible();
    // Первая отрисовка: заливка уже на месте, переход не идёт.
    expect(await fill.evaluate((node) => node.getAnimations().length)).toBe(0);
    const before = await fill.evaluate((node) => ({ width: getComputedStyle(node).width, track: getComputedStyle(node.parentElement!).width, x: new DOMMatrix(getComputedStyle(node).transform).m41 }));
    // Ширина заливки не меняется: показание едет переносом.
    expect(before.width).toBe(before.track);
    const five = claude(api).five_hour!;
    hub.append('conversations', {
      kind: 'limits_changed',
      payload: { provider: 'claude', window: 'five_hour', used_permille: 900, resets_at: five.resets_at, observed_at: new Date().toISOString(), weekly_reserve_active: false, weekly_reserve_resets_at: null }
    });
    hub.flush();
    await expect(page.locator('.limits').first().locator('.row').nth(0).locator('.meter').first()).toContainText('10%');
    const during = await fill.evaluate((node) => node.getAnimations().map((animation) => (animation as CSSTransition).transitionProperty));
    if (motion === 'reduced') expect(during).toEqual([]);
    else expect(during).toEqual(['transform']);
    await expect.poll(() => fill.evaluate((node) => node.getAnimations().length), { timeout: 3_000 }).toBe(0);
    const after = await fill.evaluate((node) => ({ width: getComputedStyle(node).width, x: new DOMMatrix(getComputedStyle(node).transform).m41 }));
    expect(after.width).toBe(before.width);
    expect(after.x).toBeLessThan(before.x);
  });
}

interface StripGeometry {
  strip: { left: number; right: number; top: number };
  rows: { left: number; right: number; striped: boolean; nameInset: number; contentRight: number }[];
  chevron: { left: number; right: number; top: number; bottom: number; centerY: number };
  firstLine: { top: number; bottom: number };
  updatedLeft: number | null;
  linkRight: number | null;
  overflow: number;
}

async function stripGeometry(page: Page): Promise<StripGeometry> {
  return page.locator('.limits').first().evaluate((root) => {
    const box = (el: Element) => el.getBoundingClientRect();
    const summary = box(root.querySelector('summary')!);
    const rows = [...root.querySelectorAll<HTMLElement>('.row')].map((row) => {
      const r = box(row);
      const name = box(row.querySelector('.pname')!);
      // Правый край содержимого: самая правая видимая ячейка строки, кроме стрелки.
      const cells = [...row.querySelectorAll<HTMLElement>('.tag, .bar, .pct, .reset, .none, .reserve, .estimated')]
        .filter((cell) => cell.getClientRects().length > 0)
        .map((cell) => box(cell).right);
      return {
        left: r.left,
        right: r.right,
        striped: getComputedStyle(row).backgroundColor !== getComputedStyle(root.querySelector('.rows')!).backgroundColor,
        nameInset: name.left - r.left,
        contentRight: Math.max(...cells)
      };
    });
    const chevron = box(root.querySelector('.chevron svg')!);
    const firstName = box(root.querySelector('.row .pname')!);
    const updated = root.querySelector('.updated');
    const link = root.querySelector('.status a');
    const visible = (el: Element | null) => (el && el.getClientRects().length > 0 ? el : null);
    return {
      strip: { left: summary.left, right: summary.right, top: summary.top },
      rows,
      chevron: { left: chevron.left, right: chevron.right, top: chevron.top, bottom: chevron.bottom, centerY: chevron.top + chevron.height / 2 },
      firstLine: { top: firstName.top, bottom: firstName.bottom },
      updatedLeft: visible(updated) ? box(updated!).left : null,
      linkRight: visible(link) ? box(link!).right : null,
      overflow: document.documentElement.scrollWidth - document.documentElement.clientWidth
    };
  });
}

/** Фон чередующихся строк идёт на всю ширину полосы, текст отступает от его края, стрелка стоит в первой строке. */
function expectStripGeometry(g: StripGeometry): void {
  expect(g.overflow).toBeLessThanOrEqual(0);
  expect(g.rows.some((row) => row.striped)).toBe(true);
  for (const row of g.rows) {
    expect(Math.abs(row.left - g.strip.left), 'stripe starts at the strip edge').toBeLessThanOrEqual(1);
    expect(Math.abs(row.right - g.strip.right), 'stripe ends at the strip edge').toBeLessThanOrEqual(1);
    expect(row.nameInset, 'name is inset from the stripe').toBeGreaterThanOrEqual(8);
    // Значения не заходят под колонку стрелки и не касаются края полосы.
    expect(row.contentRight, 'values stay left of the chevron').toBeLessThanOrEqual(g.chevron.left + 0.5);
  }
  const insets = g.rows.map((row) => row.nameInset);
  for (const inset of insets) expect(Math.abs(inset - insets[0]!), 'striped and plain rows share columns').toBeLessThanOrEqual(1);
  const rightInset = g.strip.right - g.chevron.right;
  expect(rightInset, 'chevron is inset like the text').toBeGreaterThanOrEqual(insets[0]! - 4);
  expect(Math.abs(rightInset - insets[0]!), 'chevron inset matches the text inset').toBeLessThanOrEqual(4);
  expect(g.chevron.centerY, 'chevron sits on the first row').toBeGreaterThanOrEqual(g.firstLine.top);
  expect(g.chevron.centerY, 'chevron sits on the first row').toBeLessThanOrEqual(g.firstLine.bottom);
}

test('stripes_span_the_strip_and_the_chevron_keeps_its_place_in_both_states', async ({ page }) => {
  await boot(page, fourProviders);
  const strip = page.locator('.limits').first();
  const widths = test.info().project.name === 'phone' ? [320, 360, 390] : [1280, 1440];
  // Поворот стрелки и смена темы идут переходами: геометрию меряем после них.
  const settled = () => expect.poll(() => strip.evaluate((node) => node.getAnimations({ subtree: true }).length)).toBe(0);
  for (const width of widths) {
    await resizeViewport(page, { width, height: 900 });
    for (const theme of ['dark', 'light']) {
      await setListTheme(page, theme as 'dark' | 'light');
      if (await strip.evaluate((node) => (node as HTMLDetailsElement).open)) await strip.locator('summary').click();
      await settled();
      // Свёрнутая полоса — плашки в один ряд, раскрытая — полосы строк на всю ширину.
      await expectPlaques(page, FOUR_PROVIDERS);
      await strip.locator('summary').click();
      await expect(strip.locator('.reset-short:visible')).toHaveCount(0);
      await settled();
      const expanded = await stripGeometry(page);
      expectStripGeometry(expanded);
      // Подвал стоит по тем же краям, что и строки.
      expect(Math.abs(expanded.updatedLeft! - (expanded.rows[0]!.left + expanded.rows[0]!.nameInset)), `${width} footer left`).toBeLessThanOrEqual(1);
      expect(Math.abs(expanded.linkRight! - expanded.chevron.right), `${width} footer right`).toBeLessThanOrEqual(1);
      await shot(page, `strip-geometry-${width}`);
    }
  }
});

test('a_provider_gone_from_the_machine_catalog_leaves_the_strip_and_returns_with_it', async ({ page }) => {
  const { hub } = await boot(page, fourProviders);
  const strip = await openStrip(page);
  await expect(strip.locator('.row')).toHaveCount(4);
  await expect(strip.locator('.row').filter({ hasText: 'Antigravity' })).toHaveCount(1);
  hub.append('conversations', { kind: 'limits_providers_changed', payload: { providers: ['claude', 'codex', 'opencode'] } });
  hub.flush();
  await expect(strip.locator('.row')).toHaveCount(3);
  await expect(strip.locator('.row').filter({ hasText: 'Antigravity' })).toHaveCount(0);
  await expect(strip.locator('.row').filter({ hasText: 'OpenCode' })).toHaveCount(1);
  const observed = new Date().toISOString();
  hub.append('conversations', { kind: 'limits_providers_changed', payload: { providers: ['antigravity', 'claude', 'codex', 'opencode'] } });
  hub.append('conversations', {
    kind: 'limits_changed',
    payload: { provider: 'antigravity', window: 'weekly', used_permille: 250, resets_at: new Date(Date.now() + 86_400_000).toISOString(), observed_at: observed, weekly_reserve_active: false, weekly_reserve_resets_at: null }
  });
  hub.flush();
  await expect(strip.locator('.row').filter({ hasText: 'Antigravity' })).toHaveCount(1);
  await expect(strip.locator('.row').filter({ hasText: 'Antigravity' }).locator('.meter[data-window="week"]')).toContainText('75%');
});

function providerStatus(level: ProviderStatus['level']): ProviderStatus {
  return {
    level,
    description: 'Partial System Outage',
    components: ['Claude Code'],
    incidents: [{ name: 'Elevated errors on Claude Code', url: 'https://status.claude.com/incidents/x' }],
    url: 'https://status.claude.com',
    fetched_at: new Date().toISOString(),
    changed_at: new Date().toISOString(),
    stale: false
  };
}

/** Цвет токена в той же теме: временный элемент внутри полосы. */
async function tokenColor(page: Page, token: string): Promise<string> {
  return page.locator('.limits').first().evaluate((node, name) => {
    const probe = document.createElement('span');
    probe.style.color = `var(${name})`;
    node.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  }, token);
}

test('provider_outage_is_named_in_words_and_opens_the_status_page', async ({ page }) => {
  await boot(page, (api) => {
    claude(api).status = providerStatus('partial_outage');
    const codex = api.limits.find((item) => item.provider === 'codex');
    if (codex) codex.status = { ...providerStatus('ok'), url: 'https://status.openai.com' };
  });
  await page.context().route('https://status.claude.com/**', (route) => route.fulfill({ contentType: 'text/html', body: '<title>status</title>' }));
  const plaques = page.locator('[data-limit-plaques] .plaque');
  const label = plaques.nth(0).locator('.pstatus');
  await expect(label).toHaveText('частичный сбой');
  await expect(label).toHaveAttribute('data-status-tone', 'danger');
  // В свёрнутой полосе слово — простой текст: единственный элемент управления — раскрытие.
  expect(await label.evaluate((node) => node.tagName)).toBe('SPAN');
  await expect(plaques.nth(0).locator('a')).toHaveCount(0);
  expect(await label.evaluate((node) => getComputedStyle(node).color)).toBe(await tokenColor(page, '--danger'));
  await expect(plaques.nth(1).locator('.pstatus')).toHaveCount(0);
  // Касание слова раскрывает полосу и никуда не ведёт.
  let popupOpened = false;
  const popup = page.waitForEvent('popup', { timeout: 1_000 }).then(() => { popupOpened = true; }).catch(() => {});
  await label.click();
  await expect(page.locator('.limits').first()).toHaveJSProperty('open', true);
  await popup;
  expect(popupOpened).toBe(false);
  expect(page.url()).not.toContain('status.claude.com');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, 'strip-status-collapsed');
  // Раскрытая полоса: строка провайдера со ссылкой на страницу статуса, компанией и подробностями.
  const strip = await openStrip(page);
  const rowLink = strip.locator('.row').nth(0).locator('.pstatus');
  await expect(rowLink).toHaveText('Anthropic: частичный сбой');
  await expect(rowLink).toHaveAttribute('href', 'https://status.claude.com');
  await expect(rowLink).toHaveAttribute('target', '_blank');
  await expect(rowLink).toHaveAttribute('aria-label', /^Anthropic: частичный сбой/);
  await expect(strip.locator('.row').nth(1).locator('.pstatus')).toHaveCount(0);
  const detail = strip.locator('.status-detail[data-provider="claude"]');
  await expect(detail).toContainText('Partial System Outage');
  await expect(detail.getByRole('link', { name: 'Elevated errors on Claude Code' })).toHaveAttribute('href', 'https://status.claude.com/incidents/x');
  await expect(strip.locator('.status-detail')).toHaveCount(1);
  await shot(page, 'strip-status-open');
});

test('provider_status_label_follows_live_events_and_hides_when_ok', async ({ page }) => {
  const { hub } = await boot(page);
  await expect(page.locator('.limits .pstatus')).toHaveCount(0);
  const send = (level: ProviderStatus['level']) => {
    hub.append('conversations', { kind: 'provider_status_changed', payload: { provider: 'codex', status: { ...providerStatus(level), url: 'https://status.openai.com' } } });
    hub.flush();
  };
  send('degraded');
  const label = page.locator('[data-limit-plaques] .plaque').nth(1).locator('.pstatus');
  await expect(label).toHaveText('замедление');
  await expect(label).toHaveAttribute('data-status-tone', 'warning');
  expect(await label.evaluate((node) => node.tagName)).toBe('SPAN');
  expect(await label.evaluate((node) => getComputedStyle(node).color)).toBe(await tokenColor(page, '--warning'));
  send('major_outage');
  await expect(label).toHaveText('сбой');
  await expect(label).toHaveAttribute('data-status-tone', 'danger');
  send('ok');
  await expect(page.locator('.limits .pstatus')).toHaveCount(0);
  send('maintenance');
  await expect(label).toHaveText('обслуживание');
  send('unknown');
  await expect(page.locator('.limits .pstatus')).toHaveCount(0);
});
