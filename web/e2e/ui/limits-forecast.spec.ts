import type { Page } from '@playwright/test';
import { mkdirSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { DEEPSEEK, PROJECTED_SKIP, seedRoutingCatalog, seedRoutingWhitelist } from '../../testing/fixtures/routing';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

// Прогноз расхода лимитов и отсчёт до сброса: полоса, экран лимитов, редактор и предпросмотр маршрута.
const OWNER = '00000000-0000-4000-8000-0000000000d7';
const N = '\u00a0';
const SHOTS = process.env.FORECAST_SHOTS_DIR;

/** Снимки на ревью: телефон 390×844 в тёмной теме, рабочий стол 1280×800 в светлой. */
function look(): { theme: 'dark' | 'light'; viewport: { width: number; height: number } } {
  return test.info().project.name === 'phone'
    ? { theme: 'dark', viewport: { width: 390, height: 844 } }
    : { theme: 'light', viewport: { width: 1280, height: 800 } };
}

async function prepare(page: Page): Promise<void> {
  const { theme, viewport } = look();
  await page.setViewportSize(viewport);
  await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
}

async function shot(page: Page, name: string, what: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  const { theme, viewport } = look();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
  const file = `${name}-${test.info().project.name}-${viewport.width}x${viewport.height}-${theme}.png`;
  await page.screenshot({ path: join(SHOTS, file) });
  const index = join(SHOTS, 'index.txt');
  const lines = existsSync(index) ? readFileSync(index, 'utf8').split('\n').filter(Boolean) : [];
  writeFileSync(index, [...lines.filter((line) => !line.startsWith(`${file} `)), `${file} — ${what}`].sort().join('\n') + '\n');
}

async function bootList(page: Page, patch?: (api: FakeHubApi) => void) {
  await prepare(page);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  patch?.(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

test('collapsed_strip_counts_down_to_each_shown_reset_and_tints_a_window_that_runs_out_first', async ({ page }) => {
  await bootList(page);
  const strip = page.locator('.limits').first();
  // Свёрнутая полоса — плашка на провайдера: окно, где осталось меньше всего, и отсчёт до его сброса.
  const plaques = strip.locator('[data-limit-plaques] .plaque');
  const week = plaques.nth(0).locator('.plaque-reset');
  const codex = plaques.nth(1).locator('.plaque-reset');
  await expect(week.locator('.reset-short')).toBeVisible();
  // Неделя Claude по прогнозу кончится раньше сброса: отсчёт окрашен, слова — в подсказке и раскрытой полосе.
  await expect(week).toHaveAttribute('data-forecast', 'before');
  await expect(week).toHaveAttribute('title', /кончится через 3 часа, раньше сброса$/);
  await expect(codex).not.toHaveAttribute('data-forecast', /.*/);
  const tones = await strip.evaluate((root) => {
    const [claude, other] = root.querySelectorAll('.plaque-reset');
    return { week: getComputedStyle(claude!).color, codex: getComputedStyle(other!).color };
  });
  expect(tones.week).not.toBe(tones.codex);
  // Чтение вслух: полный отсчёт с верными формами числа и прогноз.
  await expect(week.locator('.sr-only')).toHaveText(/^, сброс через 23 часа 5\d минут(ы|у)?, кончится через 3 часа$/);
  await expect(strip.locator('.forecast:visible')).toHaveCount(0);
  // Плашки стоят в один ряд.
  const tops = await plaques.evaluateAll((items) => items.map((item) => Math.round(item.getBoundingClientRect().top)));
  expect(new Set(tops).size).toBe(1);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, '01-sessions-strip-collapsed', 'список сессий, свёрнутая полоса лимитов: отсчёт до сброса у каждого окна, неделя Claude окрашена прогнозом');
});

test('expanded_strip_spells_the_reset_and_warns_only_for_a_window_that_runs_out_before_it', async ({ page }) => {
  await bootList(page);
  const strip = page.locator('.limits').first();
  await strip.locator('summary').click();
  const claude = strip.locator('.row').nth(0);
  const warning = claude.locator('.meter[data-window="week"] .forecast');
  await expect(warning).toHaveText(`кончится через 3${N}ч`);
  // Спокойный прогноз в полосе не пишется: только предупреждение.
  await expect(strip.locator('.forecast')).toHaveCount(1);
  await expect(strip.locator('.forecast')).not.toContainText('хватит');
  // Цвет предупреждения — тот же токен, что у отсчёта окна в свёрнутой полосе.
  const muted = await claude.locator('.meter[data-window="five"] .reset').evaluate((node) => getComputedStyle(node).color);
  expect(await warning.evaluate((node) => getComputedStyle(node).color)).not.toBe(muted);
  await expect(claude.locator('.meter[data-window="five"] .reset')).toContainText(/(59\u00a0мин|1\u00a0ч)$/);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, '02-sessions-strip-expanded', 'список сессий, раскрытая полоса: сброс словами и предупреждение «кончится через 3 ч» у недели Claude');
});

test('chat_header_strip_shows_the_countdown_collapsed_and_the_forecast_expanded', async ({ page }) => {
  await prepare(page);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  const id = seedConversation(api, { title: 'Прогноз лимитов', harness: { id: 'claude', title: 'Claude', provider: 'claude' } });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const strip = page.locator('.chat-header .limits');
  await expect(strip.locator('.row')).toHaveCount(1);
  await expect(strip.locator('.reset-short:visible')).toHaveCount(2);
  await expect(strip.locator('.meter[data-window="week"] .reset')).toHaveAttribute('data-forecast', 'before');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shot(page, '03-chat-header-strip-collapsed', 'шапка чата, свёрнутая полоса: отсчёт до сброса 5ч и недели');
  await strip.locator('summary').click();
  await expect(strip.locator('.forecast')).toHaveText(`кончится через 3${N}ч`);
  await shot(page, '04-chat-header-strip-expanded', 'шапка чата, раскрытая полоса: сброс словами и прогноз недели');
});

test('countdown_ticks_with_the_shared_clock_every_minute', async ({ page }) => {
  await prepare(page);
  const start = Date.now();
  await page.clock.install({ time: start });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  const claude = api.limits.find((row) => row.provider === 'claude')!;
  // Сброс через 3 минуты 30 секунд, показание свежее на всё время теста.
  claude.five_hour = { ...claude.five_hour!, resets_at: new Date(start + 3 * 60_000 + 30_000).toISOString(), observed_at: new Date(start).toISOString(), forecast: null };
  // Неделя почти полна: плашка Claude показывает пятичасовое окно.
  claude.weekly = { ...claude.weekly!, used_permille: 100, observed_at: new Date(start).toISOString(), forecast: null };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const strip = page.locator('.limits').first();
  const plaque = strip.locator('[data-limit-plaques] .plaque').nth(0).locator('.plaque-reset');
  const reset = strip.locator('.row').nth(0).locator('.meter[data-window="five"] .reset');
  await expect(plaque.locator('.reset-short')).toHaveText('3м');
  await page.clock.runFor(60_000);
  await expect(plaque.locator('.reset-short')).toHaveText('2м');
  await page.clock.runFor(2 * 60_000);
  await expect(plaque.locator('.reset-short')).toHaveText('<1м');
  await strip.locator('summary').click();
  await expect(reset.locator('.sr-only')).toHaveText('сброс меньше чем через минуту');
  // Сброс прошёл: отсчёт исчезает, окно называется устаревшим, а не «сбрасывается» без конца.
  await page.clock.runFor(31_000);
  await expect(reset).toHaveCount(0);
  await expect(strip.locator('.row').nth(0).locator('.meter[data-window="five"]')).toContainText('устарело');
});

test('limits_screen_shows_the_countdown_the_warning_the_calm_forecast_and_nothing_without_a_forecast', async ({ page }) => {
  await prepare(page);
  await bootSecondary(page, '/limits');
  await page.locator('main[data-secondary-ready]').waitFor();
  const claude = page.getByRole('region', { name: 'Claude, текущие лимиты' });
  const codex = page.getByRole('region', { name: 'Codex, текущие лимиты' });
  const claudeWeek = claude.locator('.row[data-window="week"]');
  await expect(claudeWeek.locator('.reset')).toHaveText(/^сброс через 23\u00a0ч 5\d\u00a0мин$/);
  await expect(claudeWeek.locator('.forecast')).toHaveAttribute('data-forecast', 'before');
  await expect(claudeWeek.locator('.verdict')).toHaveText(`кончится через 3${N}ч`);
  await expect(claudeWeek.locator('.pace')).toHaveText(`≈1${N}%/ч`);
  // Спокойный прогноз — только здесь: окна хватит до сброса, рядом средняя скорость.
  const claudeFive = claude.locator('.row[data-window="five"]');
  await expect(claudeFive.locator('.forecast')).toHaveAttribute('data-forecast', 'calm');
  await expect(claudeFive.locator('.verdict')).toHaveText('хватит до сброса');
  await expect(claudeFive.locator('.pace')).toHaveText(`≈12${N}%/ч`);
  // Хаб не прислал прогноза: строки нет вовсе.
  await expect(codex.locator('.row[data-window="five"] .forecast')).toHaveCount(0);
  await expect(codex.locator('.row[data-window="five"] .reset')).toHaveText(/^сброс через (59\u00a0мин|1\u00a0ч)$/);
  const tones = await claude.evaluate((card) => ({
    before: getComputedStyle(card.querySelector('.row[data-window="week"] .verdict')!).color,
    calm: getComputedStyle(card.querySelector('.row[data-window="five"] .verdict')!).color
  }));
  expect(tones.before).not.toBe(tones.calm);
  // Полная дата сброса — в общей подсказке времени.
  const time = claudeWeek.locator('time.reset');
  await expect(time).toHaveAttribute('aria-label', /^сброс через 23 часа 5\d минут/);
  await time.focus();
  await expect(page.locator('.time-tip')).toHaveText(/\d{4}, \d\d:\d\d:\d\d$/);
  await page.keyboard.press('Escape');
  await time.blur();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await claude.scrollIntoViewIfNeeded();
  await shot(page, '05-limits-screen-warning-and-calm', 'экран лимитов: неделя Claude «кончится через 3 ч» (предупреждение), 5 часов «хватит до сброса», у Codex 5ч без прогноза');
});

test('limits_screen_calm_case_when_every_window_lasts_until_the_reset', async ({ page }) => {
  await prepare(page);
  await bootSecondary(page, '/limits', (api) => {
    const claude = api.limits.find((row) => row.provider === 'claude')!;
    claude.weekly = { ...claude.weekly!, used_permille: 610, forecast: { rate_permille_per_hour: 9, recent_rate_permille_per_hour: 14, exhausts_at: new Date(Date.now() + 43 * 3_600_000).toISOString(), before_reset: false } };
  });
  await page.locator('main[data-secondary-ready]').waitFor();
  const forecasts = page.locator('.current .forecast');
  await expect(forecasts).toHaveCount(3);
  await expect(page.locator('.current .forecast[data-forecast="before"]')).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Claude, текущие лимиты' }).locator('.row[data-window="week"] .verdict')).toHaveText('хватит до сброса');
  await shot(page, '06-limits-screen-calm', 'экран лимитов: все прогнозы спокойные, «хватит до сброса» и скорость');
});

async function bootEditor(page: Page) {
  await prepare(page);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedRoutingCatalog(api);
  seedRoutingWhitelist(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/settings/routing');
  await expect(page.locator('[data-routing-editor][data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

type SavedPolicy = { task_types: Record<string, { expected_minutes?: number }> };

test('policy_editor_saves_the_expected_task_length_and_rejects_out_of_range', async ({ page }) => {
  const { api } = await bootEditor(page);
  const bugfix = page.locator('[data-task-key="bugfix"]');
  const review = page.locator('[data-task-key="review"]');
  const field = (task: typeof bugfix) => task.getByLabel('Ожидаемая длительность, мин');
  await expect(field(bugfix)).toHaveValue('90');
  // Не задано: подсказка показывает умолчание хаба.
  await expect(field(review)).toHaveValue('');
  await expect(field(review)).toHaveAttribute('placeholder', '60');
  await field(review).fill('3');
  await expect(review.getByRole('alert')).toHaveText('Ожидаемая длительность: от 5 до 1440 минут');
  await expect(page.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
  await field(review).fill('45');
  await expect(review.getByRole('alert')).toHaveCount(0);
  await field(bugfix).fill('');
  await field(review).scrollIntoViewIfNeeded();
  await shot(page, '07-policy-editor-expected-minutes', 'редактор маршрутизации: поле «Ожидаемая длительность, мин» у типа задачи');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.locator('[data-role="policy-status"]')).toContainText('ревизия 1');
  const saved = api.routingPolicy as SavedPolicy;
  expect(saved.task_types.review!.expected_minutes).toBe(45);
  // Пустое поле не сохраняется: хаб возьмёт умолчание.
  expect(saved.task_types.bugfix).not.toHaveProperty('expected_minutes');
  await page.reload();
  await expect(field(page.locator('[data-task-key="review"]'))).toHaveValue('45');
});

test('routing_preview_translates_the_projected_threshold_reason', async ({ page }) => {
  const { api } = await bootEditor(page);
  api.routingSkips.set(`opencode/${DEEPSEEK}`, { ...PROJECTED_SKIP });
  const preview = page.locator('[data-role="routing-preview"]');
  await preview.getByRole('button', { name: 'Проверить' }).click();
  const result = preview.locator('[data-role="preview-result"]');
  await expect(result.locator('[data-role="route-chosen"]')).toHaveText('Opus 1M · высокое');
  await expect(result).toContainText('превысит порог');
  await expect(result).toContainText(`неделя 72${N}%, через 60${N}мин будет ~84${N}% при пороге 80${N}%`);
  await expect(result).not.toContainText('projected');
  await result.scrollIntoViewIfNeeded();
  await shot(page, '08-routing-preview-projected-reason', 'предпросмотр маршрута: кандидат пропущен по прогнозу, причина переведена');

  const reset = Date.now() + 2 * 3_600_000 + 15 * 60_000 + 30_000;
  api.routingRefusal = { candidates: [{ harness: 'codex', model: 'gpt-6.1-sol', ...PROJECTED_SKIP }], nearest_reset: new Date(reset).toISOString() };
  await preview.getByLabel('Тип задачи').selectOption('review');
  await preview.getByRole('button', { name: 'Проверить' }).click();
  const refused = preview.locator('[data-role="preview-refused"]');
  await expect(refused.locator('li')).toContainText(`неделя 72${N}%, через 60${N}мин будет ~84${N}% при пороге 80${N}%`);
  await expect(refused).toContainText(`(через 2${N}ч 15${N}мин)`);
});
