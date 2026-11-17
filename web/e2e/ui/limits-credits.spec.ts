import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const DAY = 86_400_000;

/** Дата в стиле подсказок: «12.12». */
function dayLabel(ts: number): string {
  return new Intl.DateTimeFormat('ru-RU', { day: '2-digit', month: '2-digit' }).format(ts);
}

/** Дата цифрами: «02.12.2026». */
function fullDay(ts: number): string {
  return new Intl.DateTimeFormat('ru-RU', { day: '2-digit', month: '2-digit', year: 'numeric' }).format(ts);
}

type Credit = { title: string | null; expires_at: string | null };

function credits(count: number, resets: Credit[]) {
  return { reset_count: count, resets, observed_at: new Date().toISOString() };
}

async function boot(page: Page, codexCredits: ReturnType<typeof credits> | null) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  api.limits.find((row) => row.provider === 'codex')!.credits = codexCredits;
  await installChatRoutes(page, hub, api, OWNER);
  return { hub, api, ids };
}

async function openStrip(page: Page) {
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const strip = page.locator('.limits').first();
  await expect(strip).toBeVisible();
  if (!(await strip.evaluate((node) => (node as HTMLDetailsElement).open))) await strip.locator('summary').click();
  return strip;
}

async function tokenColor(page: Page, strip: ReturnType<Page['locator']>, token: string): Promise<string> {
  return strip.evaluate((node, name) => {
    const probe = document.createElement('span');
    probe.style.color = `var(${name})`;
    node.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  }, token);
}

test('reset_credits_are_named_on_the_limits_strip_with_the_nearest_expiry', async ({ page }) => {
  const far = Date.now() + 60 * DAY;
  await boot(page, credits(2, [{ title: 'Full reset (Weekly + 5 hr)', expires_at: new Date(far).toISOString() }]));
  const strip = await openStrip(page);
  const line = strip.locator('.credit[data-provider="codex"]');
  await expect(line).toHaveText(`Сбросы лимитов: 2, ближайший сгорит ${dayLabel(far)}`);
  await expect(line).toHaveAttribute('data-tone', 'normal');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test('a_reset_burning_within_a_week_warns_on_the_strip', async ({ page }) => {
  const soon = Date.now() + 3 * DAY;
  await boot(page, credits(1, [{ title: 'Full reset (Weekly + 5 hr)', expires_at: new Date(soon).toISOString() }]));
  const strip = await openStrip(page);
  const line = strip.locator('.credit[data-provider="codex"]');
  await expect(line).toHaveText(`Сбросы лимитов: 1, ближайший сгорит ${dayLabel(soon)}`);
  await expect(line).toHaveAttribute('data-tone', 'warning');
  expect(await line.evaluate((node) => getComputedStyle(node).color)).toBe(await tokenColor(page, strip, '--warning'));
});

test('known_resets_without_any_expiry_show_only_the_count', async ({ page }) => {
  await boot(page, credits(2, [{ title: 'Full reset (Weekly + 5 hr)', expires_at: null }]));
  const strip = await openStrip(page);
  const line = strip.locator('.credit[data-provider="codex"]');
  await expect(line).toHaveText('Сбросы лимитов: 2');
  await expect(line).toHaveAttribute('data-tone', 'normal');
});

test('no_credits_or_zero_available_resets_leave_no_line', async ({ page }) => {
  await boot(page, credits(0, []));
  const strip = await openStrip(page);
  await expect(strip.locator('.credit')).toHaveCount(0);
});

test('the_limits_screen_lists_each_reset_by_name_and_burning_date', async ({ page }) => {
  const burning = Date.now() + 20 * DAY;
  await bootSecondary(page, '/limits', (api) => {
    api.limits.find((row) => row.provider === 'codex')!.credits = credits(2, [
      { title: 'Full reset (Weekly + 5 hr)', expires_at: new Date(burning).toISOString() },
      { title: null, expires_at: null }
    ]);
  });
  await page.locator('main[data-secondary-ready]').waitFor();
  const codex = page.getByRole('region', { name: 'Codex, текущие лимиты' });
  await expect(codex.locator('.credits[aria-label="Сбросы лимитов"] li')).toHaveText([
    `Полный сброс: неделя и 5 часов\u00a0— сгорит ${fullDay(burning)}`,
    'Сброс лимитов'
  ]);
  // У Claude сбросов нет: списка и подписи у его карточки тоже нет.
  const claude = page.getByRole('region', { name: 'Claude, текущие лимиты' });
  await expect(claude.locator('.credits')).toHaveCount(0);
});
