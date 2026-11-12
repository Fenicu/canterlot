// Снимки состояний сессий и экрана настроек для просмотра владельцем: стол 1920 и 2560, телефон.
// Как остальные снимки, идут только с PW_SCREENSHOTS=1; файлы ложатся в SESSION_STATES_OUT (по умолчанию test-results/session-states).
import { test, expect, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const OUT = resolve(process.env.SESSION_STATES_OUT ?? 'test-results/session-states');
const NOW_MS = Math.floor(Date.now() / 1000) * 1000;
const NOW = new Date(NOW_MS).toISOString();
const stamp = (minutes: number) => new Date(NOW_MS + minutes * 60_000).toISOString();

const WS = { id: '00000000-0000-4000-8000-000000000101', name: 'workstation' };
const CLAUDE = { id: 'claude', title: 'Claude Code', provider: 'claude' };
const CODEX = { id: 'codex', title: 'Codex', provider: 'codex' };
const OPENCODE = { id: 'opencode', title: 'OpenCode', provider: 'opencode' };
const HARNESSES = [
  { id: 'claude', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['high'] }], permission_modes: ['owner'] },
  { id: 'codex', title: 'Codex', provider: 'codex', models: [{ id: 'gpt-6.1-sol', title: 'gpt-6.1-sol', efforts: ['high'] }], permission_modes: ['owner'] },
  { id: 'opencode', title: 'OpenCode', provider: 'opencode', models: [{ id: 'deepseek', title: 'deepseek', efforts: ['high'] }], permission_modes: ['owner'] }
];

function seed(api: FakeHubApi): { open: string; machine: string; pinned: string } {
  seedMachine(api, { id: WS.id, name: WS.name, availability: 'ready', os: 'linux', harnesses: HARNESSES });
  api.limits = [
    { provider: 'claude', weekly_reserve_active: false, five_hour: { used_permille: 610, resets_at: stamp(110), observed_at: stamp(-1) }, weekly: { used_permille: 845, resets_at: stamp(2 * 24 * 60), observed_at: stamp(-1) } },
    { provider: 'codex', weekly_reserve_active: false, five_hour_absent: true, weekly: { used_permille: 420, resets_at: stamp(4 * 24 * 60), observed_at: stamp(-1) } }
  ] as never;
  const conv = (title: string, harness: typeof CLAUDE, model: string, ago: number, activity: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
    seedConversation(api, { title, harness, model, machine: WS, created_at: stamp(-300), last_activity_at: stamp(-ago), activity: activity as never, ...extra } as never);

  const pinned = conv('Скрипт выкатки', CLAUDE, 'opus-5.5', 1, { state: 'working', evidence: { kind: 'working', phase: 'generating' }, since: stamp(-3) }, { pinned: true, color: 'accent' });
  conv('Темы оформления', OPENCODE, 'deepseek', 4, { state: 'waiting_for_user', evidence: { kind: 'waiting_for_user', question: 'Какую палитру взять за основу?' }, since: stamp(-4) }, { unread_count: 1 });
  conv('Адаптер Antigravity', CODEX, 'gpt-6.1-sol', 2, { state: 'working', evidence: { kind: 'working', phase: 'running_tool', tool_name: 'Bash', summary: 'cargo test', phase_since: stamp(-6) }, since: stamp(-8) });
  const orchestrator = conv('Выпуск 1.31', CLAUDE, 'opus-5.5', 5, { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-5), process_alive: true }, since: stamp(-5) }, { preview: 'Раздал три задачи делегатам, жду результатов по поиску и пушам.', color: 'blue' });
  const child = (title: string, harness: typeof CLAUDE, model: string, activity: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
    conv(title, harness, model, 6, activity, { parent_id: orchestrator, spawn_depth: 1, working_ms: 1_380_000, ...extra });
  child('Пуши Gotify', CLAUDE, 'opus-5.5', { state: 'working', evidence: { kind: 'working', phase: 'generating' }, since: stamp(-2) });
  child('Поиск по истории', CODEX, 'gpt-6.1-sol', { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-9), process_alive: true }, since: stamp(-9) }, { preview: 'Поиск готов: индекс по ответам, подсветка совпадений.' });
  child('Доска плана', CLAUDE, 'opus-5.5', { state: 'stopped', evidence: { kind: 'stopped', outcome: 'normal_exit' }, since: stamp(-40) }, { preview: 'Доска плана готова, тесты зелёные.' });
  child('Смена effort на ходу', CLAUDE, 'opus-5.5', { state: 'failed', evidence: { kind: 'failed', reason: 'process_exited' }, since: stamp(-24) });
  const open = conv('Ревью ветки', CLAUDE, 'opus-5.5', 12, { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-12), process_alive: true }, since: stamp(-12) }, { preview: 'Замечаний два: гонка в очереди и лишний клон в горячем пути.', color: 'amber' });
  conv('Разбор логов', CODEX, 'gpt-6.1-sol', 25, { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-25), process_alive: false }, since: stamp(-25) }, { preview: 'Нашёл три повтора подряд.' });
  conv('Сторож машины', OPENCODE, 'deepseek', 18, { state: 'stalled', evidence: { kind: 'stalled', last_event_at: stamp(-18), phase: 'generating', process_alive: true, cpu_busy: false, link: 'available' }, since: stamp(-18) });
  conv('Миграция схемы', CLAUDE, 'opus-5.5', 20, { state: 'quota_paused', evidence: { kind: 'quota_paused', reason: 'quota_exhausted', provider: 'claude', window: 'five_hour', until: stamp(110) }, since: stamp(-20) }, { queue: { queued: 1, pause_reasons: [], hold_reasons: [] } });
  conv('Сборка образа воркера', CODEX, 'gpt-6.1-sol', 35, { state: 'failed', evidence: { kind: 'failed', reason: 'process_exited' }, since: stamp(-35) });
  conv('Черновик README', CLAUDE, 'opus-5.5', 60, { state: 'stopped', evidence: { kind: 'stopped', outcome: 'user_stop' }, since: stamp(-60) }, { preview: 'Раздел про установку переписан.' });
  conv('Старый эксперимент', OPENCODE, 'deepseek', 300, { state: 'stopped', evidence: { kind: 'stopped', outcome: 'normal_exit' }, since: stamp(-300) }, { preview: 'Готово.' });
  conv('Новая сессия', CODEX, 'gpt-6.1-sol', 0, { state: 'idle', evidence: { kind: 'idle', last_turn_at: null, process_alive: true }, since: stamp(0) });
  api.counts = { all: 13, pinned: 1, archived: 0, trashed: 0, groups: [] } as never;

  // Немного ленты для открытого чата.
  const run = api.id();
  const user: HistoryInput = { kind: 'input', id: api.id(), idempotency_key: 'r-1', state: 'accepted', content: [{ type: 'text', text: 'Посмотри ветку с очередью перед слиянием.' }], attachments: [], created_seq: '1', feed_seq: '1', created_at: stamp(-14) } as HistoryInput;
  const text = 'Замечаний два: гонка в очереди и лишний клон в горячем пути.\n\n1. `queue.rs`: проверка слота и захват идут двумя запросами.\n2. `feed.rs`: `blocks.clone()` на каждый кадр.\n\nОстальное в порядке, тесты зелёные.';
  const answer: HistoryBlock = { kind: 'block', id: api.id(), block_kind: 'text', content: text, content_bytes: text.length, content_truncated: false, created_seq: '2', created_at: stamp(-12), rev: 1, state: 'final', agent: 'Claude Code', model: 'opus-5.5', tool_name: null, summary: null, parent_block_id: null, input_command_id: run, line_count: 6 } as HistoryBlock;
  api.histories.set(open, [user, answer]);
  api.blocks.set(answer.id, { text, rev: 1, state: 'final' });
  api.bindConversation(open);
  api.hub.flush();
  return { open, machine: WS.id, pinned };
}

async function prepare(page: Page, theme: 'dark' | 'light'): Promise<void> {
  await page.addInitScript(({ theme }) => {
    localStorage.setItem('canterlot.palette', 'aquamarine');
    localStorage.setItem('canterlot.theme', theme);
    localStorage.setItem('canterlot.motion', 'off');
  }, { theme });
  await page.clock.install({ time: new Date(NOW) });
}

async function shoot(page: Page, name: string): Promise<void> {
  await page.evaluate(() => document.fonts.ready);
  await page.clock.runFor(400);
  await page.evaluate(() => { delete document.documentElement.dataset.themeSwitching; document.documentElement.dataset.motion = 'off'; });
  await page.waitForTimeout(200);
  await page.screenshot({ path: resolve(OUT, name), animations: 'disabled' });
}

async function boot(page: Page, theme: 'dark' | 'light', path: string): Promise<{ api: FakeHubApi; ids: ReturnType<typeof seed> }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seed(api);
  await prepare(page, theme);
  await installChatRoutes(page, hub, api, OWNER);
  await seedSecondaryScreens(page, api, { machine: ids.machine, pinned: ids.pinned });
  await page.goto(path.replace('{open}', ids.open));
  await expect(page.locator('[data-list-ready], [data-secondary-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { api, ids };
}

test.use({ timezoneId: 'Europe/Moscow', browserName: 'chromium' });
test.beforeAll(() => mkdirSync(OUT, { recursive: true }));
test.beforeEach(({ browserName }, testInfo) => {
  test.skip(browserName !== 'chromium' || testInfo.project.name !== 'phone', 'снимки собирает проект phone');
});

test.describe('стол', () => {
  test.use({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1, isMobile: false, hasTouch: false });
  for (const theme of ['dark', 'light'] as const) {
    test(`список и чат, ${theme}`, async ({ page }) => {
      await boot(page, theme, '/c/{open}');
      await expect(page.locator('[data-session]').first()).toBeVisible();
      await shoot(page, `desktop-1920-sidebar-chat-${theme}.png`);
    });
  }
  for (const category of ['appearance', 'input', 'limits', 'routing', 'security', 'app']) {
    test(`настройки ${category}`, async ({ page }) => {
      await boot(page, 'dark', `/settings?c=${category}`);
      await expect(page.locator('[data-settings-pane]')).toBeVisible();
      await page.waitForTimeout(500);
      await shoot(page, `desktop-1920-settings-${category}-dark.png`);
    });
  }
});

test.describe('стол 2560', () => {
  test.use({ viewport: { width: 2560, height: 1440 }, deviceScaleFactor: 1, isMobile: false, hasTouch: false });
  test('список и чат 2560', async ({ page }) => {
    await boot(page, 'dark', '/c/{open}');
    await shoot(page, 'desktop-2560-sidebar-chat-dark.png');
  });
});

test.describe('телефон', () => {
  test.use({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
  test('список', async ({ page }) => {
    await boot(page, 'dark', '/sessions');
    await expect(page.locator('[data-session]').first()).toBeVisible();
    await shoot(page, 'phone-list-dark.png');
    await page.locator('[data-list-scroll]').evaluate((node) => { node.scrollTop = node.scrollHeight; });
    await shoot(page, 'phone-list-scrolled-dark.png');
  });
  test('настройки: разделы', async ({ page }) => {
    await boot(page, 'dark', '/settings');
    await shoot(page, 'phone-settings-index-dark.png');
  });
  test('настройки: ввод', async ({ page }) => {
    await boot(page, 'dark', '/settings?c=input');
    await expect(page.locator('[data-settings-pane]')).toBeVisible();
    await shoot(page, 'phone-settings-input-dark.png');
  });
});
