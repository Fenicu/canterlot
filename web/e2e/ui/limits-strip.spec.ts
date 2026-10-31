import type { Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
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
});

test('expanded_strip_puts_resets_beside_meters_and_offers_the_refresh_button', async ({ page }) => {
  await boot(page);
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  await expect(strip.locator('.reset').first()).toBeVisible();
  await expect(strip.locator('.reset').first()).toHaveText(/^сброс (\p{L}{2} )?\d\d:\d\d$/u);
  await expect(strip.locator('.reset')).toHaveCount(4);
  await expect(strip.locator('.updated')).toHaveText(/^обновлено \d\d:\d\d$/);
  await expect(strip.getByRole('button', { name: 'Обновить лимиты' })).toBeVisible();
  await expect(strip.getByRole('link', { name: 'История лимитов' })).toBeVisible();
  // Проценты второй раз не повторяются.
  await expect(strip.locator('.open')).not.toContainText('%');
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
