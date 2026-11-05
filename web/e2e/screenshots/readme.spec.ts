import { test, expect, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const OUT = resolve(import.meta.dirname, '../../../docs/screenshots');
// Часы страницы стоят, а заголовок X-Server-Time-Ms у FakeHub берётся из настоящего
// времени процесса: база «сейчас» должна совпадать с ним, иначе свежие лимиты выглядят устаревшими.
const NOW_MS = Math.floor(Date.now() / 1000) * 1000;
const NOW = new Date(NOW_MS).toISOString();

const MACHINES = {
  workstation: { id: '00000000-0000-4000-8000-000000000101', name: 'workstation' },
  mini: { id: '00000000-0000-4000-8000-000000000102', name: 'mini' }
} as const;

const HARNESSES = [
  {
    id: 'claude',
    title: 'Claude Code',
    provider: 'claude',
    models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
    permission_modes: ['owner', 'ask', 'bypass', 'read_only']
  },
  {
    id: 'codex',
    title: 'Codex',
    provider: 'codex',
    models: [{ id: 'gpt-6.1-sol', title: 'gpt-6.1-sol', efforts: ['low', 'medium', 'high'] }],
    permission_modes: ['owner', 'ask', 'bypass', 'read_only']
  },
  {
    id: 'opencode',
    title: 'OpenCode',
    provider: 'opencode',
    models: [{ id: 'deepseek', title: 'deepseek', efforts: ['low', 'medium', 'high'] }],
    permission_modes: ['owner', 'ask', 'bypass', 'read_only']
  }
];

function stamp(minutes: number): string {
  return new Date(NOW_MS + minutes * 60_000).toISOString();
}

function seedMachines(api: FakeHubApi): void {
  seedMachine(api, {
    id: MACHINES.workstation.id,
    name: MACHINES.workstation.name,
    availability: 'ready',
    os: 'linux',
    harnesses: HARNESSES
  });
  seedMachine(api, {
    id: MACHINES.mini.id,
    name: MACHINES.mini.name,
    availability: 'ready',
    os: 'linux',
    harnesses: HARNESSES
  });
}

function seedLimits(api: FakeHubApi): void {
  api.limits = [
    {
      provider: 'claude',
      weekly_reserve_active: false,
      five_hour: { used_permille: 610, resets_at: stamp(110), observed_at: stamp(-1) },
      weekly: { used_permille: 845, resets_at: stamp(2 * 24 * 60 + 5), observed_at: stamp(-1) }
    },
    {
      provider: 'codex',
      weekly_reserve_active: false,
      five_hour_absent: true,
      weekly: { used_permille: 420, resets_at: stamp(4 * 24 * 60), observed_at: stamp(-1) }
    }
  ];
}

function historyItem(api: FakeHubApi, run: string, rest: Partial<HistoryBlock> & Pick<HistoryBlock, 'block_kind' | 'content'>): HistoryBlock {
  const content = rest.content;
  return {
    kind: 'block',
    id: api.id(),
    block_kind: 'text',
    content,
    content_bytes: new TextEncoder().encode(content).length,
    content_truncated: false,
    created_seq: '2',
    created_at: stamp(-9),
    rev: 1,
    state: 'final',
    agent: 'Claude Code',
    model: 'opus-5.5',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: run,
    line_count: content.split('\n').length,
    ...rest
  };
}

function input(api: FakeHubApi, text: string, seq: string, at: number): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `readme-${seq}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: seq,
    feed_seq: seq,
    created_at: stamp(at)
  };
}

function seedReadmeSessions(api: FakeHubApi): { pinned: string; themes: string; adapter: string; review: string; machine: string } {
  seedMachines(api);
  seedLimits(api);

  const canterlot = api.id();
  const review = api.id();
  api.groups = [
    { id: canterlot, name: 'canterlot', position: 0, created_at: stamp(-7 * 24 * 60) },
    { id: review, name: 'Ревью', position: 1, created_at: stamp(-7 * 24 * 60) }
  ];

  const pinned = seedConversation(api, {
    title: 'Скрипт выкатки',
    pinned: true,
    color: 'accent',
    groups: [canterlot],
    model: 'opus-5.5',
    harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
    machine: MACHINES.workstation,
    created_at: stamp(-55),
    last_activity_at: stamp(-3),
    activity: { state: 'working', evidence: { phase: 'generating' }, since: stamp(-4) }
  });

  const themes = seedConversation(api, {
    title: 'Темы оформления',
    color: 'blue',
    groups: [canterlot],
    unread_count: 2,
    model: 'deepseek',
    harness: { id: 'opencode', title: 'OpenCode', provider: 'opencode' },
    machine: MACHINES.workstation,
    created_at: stamp(-90),
    last_activity_at: stamp(-6),
    activity: {
      state: 'waiting_for_user',
      evidence: { question: 'Какую палитру взять за основу?' },
      since: stamp(-7)
    }
  });

  const adapter = seedConversation(api, {
    title: 'Адаптер Antigravity',
    color: 'amber',
    groups: [canterlot],
    model: 'gpt-6.1-sol',
    harness: { id: 'codex', title: 'Codex', provider: 'codex' },
    machine: MACHINES.mini,
    created_at: stamp(-120),
    last_activity_at: stamp(-11),
    activity: {
      state: 'working',
      evidence: { phase: 'running_tool', tool_name: 'Bash', summary: 'cargo test', phase_since: stamp(-18) },
      since: stamp(-20)
    }
  });

  const reviewSession = seedConversation(api, {
    title: 'Ревью ветки',
    color: 'neutral',
    groups: [review],
    model: 'opus-5.5',
    harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
    machine: MACHINES.workstation,
    created_at: stamp(-180),
    last_activity_at: stamp(-42),
    activity: { state: 'stopped', evidence: { outcome: 'normal_exit' }, since: stamp(-42) }
  });

  api.counts = {
    all: 4,
    pinned: 1,
    archived: 0,
    trashed: 0,
    groups: [
      { id: canterlot, count: 3 },
      { id: review, count: 1 }
    ]
  };

  return { pinned, themes, adapter, review: reviewSession, machine: MACHINES.workstation.id };
}

function seedChat(api: FakeHubApi, existing?: string): string {
  seedMachines(api);
  seedLimits(api);
  const patch = {
    title: 'Темы оформления',
    color: 'blue',
    model: 'deepseek',
    harness: { id: 'opencode', title: 'OpenCode', provider: 'opencode' },
    machine: MACHINES.workstation,
    created_at: stamp(-38),
    last_activity_at: stamp(-7),
    activity: { state: 'waiting_for_user' as const, evidence: { question: 'Какую палитру взять за основу?' }, since: stamp(-7) }
  };
  const id = existing ?? seedConversation(api, patch);
  if (existing) {
    const row = api.list.get(existing);
    const detail = api.details.get(existing);
    if (row) Object.assign(row, patch, { unread_count: 0 });
    if (detail) Object.assign(detail, patch);
  }
  const run = api.id();
  const user = input(api, 'Обнови палитру и проверь контраст в светлой теме. Заодно прогони тесты хаба.', '1', -7);
  const answer = historyItem(api, run, {
    block_kind: 'text',
    content: 'Проверил три палитры: контраст проходит. В светлой VS Code поправил фон поля ввода.\n\n```ts\nconst palette = \'aquamarine\' as const;\nsetPalette(palette);\n```\n\nПрогоняю тесты.',
    created_seq: '2',
    created_at: stamp(-6)
  });
  const read = historyItem(api, run, {
    block_kind: 'tool_call',
    content: 'Прочитал 1 файл',
    tool_name: 'Read',
    summary: 'web/src/lib/theme.svelte.ts',
    created_seq: '3',
    created_at: stamp(-5)
  });
  const readResult = historyItem(api, run, {
    block_kind: 'tool_result',
    content: 'export function initTheme(): () => void {\n  // ...\n}',
    parent_block_id: read.id,
    created_seq: '4',
    created_at: stamp(-5)
  });
  const tests = historyItem(api, run, {
    block_kind: 'tool_call',
    content: '',
    tool_name: 'Bash',
    summary: 'cargo test -p canterlot-hub --test stream',
    created_seq: '5',
    created_at: stamp(-2)
  });
  const testsResult = historyItem(api, run, {
    block_kind: 'tool_result',
    content: 'running 3 tests\ncursor_survives_snapshot ... ok\nno_duplicates_after_reconnect ... ok\nreplay_keeps_order ... ok\ntest result: ok. 3 passed',
    parent_block_id: tests.id,
    created_seq: '6',
    created_at: stamp(-1)
  });
  api.histories.set(id, [user, answer, read, readResult, tests, testsResult]);
  for (const block of [answer, read, readResult, tests, testsResult]) {
    block.agent = 'OpenCode';
    block.model = 'deepseek';
    api.blocks.set(block.id, { text: block.content, rev: 1, state: 'final' });
  }
  api.bindConversation(id);
  api.pushContext(id, 127_600, 200_000);
  api.hub.flush();
  return id;
}

function seedRequest(api: FakeHubApi, kind: 'permission' | 'question'): string {
  seedMachines(api);
  seedLimits(api);
  const title = kind === 'permission' ? 'Сборка хаба' : 'Темы оформления';
  const id = seedConversation(api, {
    title,
    color: kind === 'permission' ? 'accent' : 'blue',
    model: kind === 'permission' ? 'opus-5.5' : 'deepseek',
    harness: kind === 'permission'
      ? { id: 'claude', title: 'Claude Code', provider: 'claude' }
      : { id: 'opencode', title: 'OpenCode', provider: 'opencode' },
    machine: MACHINES.workstation,
    created_at: stamp(-26),
    last_activity_at: stamp(-1),
    activity: { state: 'waiting_for_user', evidence: { question: kind === 'permission' ? 'Разрешение на команду' : 'Какую палитру взять за основу?' }, since: stamp(-1) }
  });
  const run = api.id();
  const user = input(api, kind === 'permission'
    ? 'Проверь сборку хаба перед выкаткой и покажи, что упало, если что-то не так.'
    : 'Посмотри ветку с темами до слияния.', '1', -8);
  const answer = historyItem(api, run, {
    block_kind: 'text',
    content: kind === 'permission'
      ? 'Сначала прогоню тесты хаба, потом соберу список изменений для выкатки.'
      : 'Ветка в порядке, осталось выбрать палитру для проверки.',
    agent: kind === 'permission' ? 'Claude Code' : 'OpenCode',
    model: kind === 'permission' ? 'opus-5.5' : 'deepseek',
    created_seq: '2',
    created_at: stamp(-7)
  });
  api.histories.set(id, kind === 'permission' ? [user, answer] : [user]);
  if (kind === 'permission') api.blocks.set(answer.id, { text: answer.content, rev: 1, state: 'final' });
  api.bindConversation(id);

  const payload = kind === 'permission'
    ? {
        tool_name: 'Bash',
        description: 'Прогнать тесты хаба перед выкаткой',
        command: 'cargo test -p canterlot-hub',
        cwd: '/srv/canterlot'
      }
    : {
        questions: [
          {
            id: 'q1',
            header: 'ТЕМЫ',
            text: 'Какую палитру взять за основу?',
            options: [
              { label: 'Аквамарин', description: 'Тёмный фон и бирюзовый акцент' },
              { label: 'Стандартная', description: 'Нейтральная, ближе к системной' },
              { label: 'VS Code', description: 'Как в редакторе, без сюрпризов' }
            ],
            multi_select: false,
            allow_custom: true
          }
        ]
      };

  api.hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    payload: { request_id: api.id(), kind, payload }
  });
  api.hub.flush();
  return id;
}

async function prepare(page: Page, palette: 'aquamarine' | 'standard' | 'vscode', theme: 'dark' | 'light'): Promise<void> {
  await page.addInitScript(({ palette, theme }) => {
    localStorage.setItem('canterlot.palette', palette);
    localStorage.setItem('canterlot.theme', theme);
    localStorage.setItem('canterlot.motion', 'off');
  }, { palette, theme });
  await page.clock.install({ time: new Date(NOW) });
}

async function settle(page: Page): Promise<void> {
  await page.evaluate(() => document.fonts.ready);
  await page.clock.runFor(400);
  await page.evaluate(() => {
    delete document.documentElement.dataset.themeSwitching;
    document.documentElement.dataset.motion = 'off';
  });
  await page.waitForTimeout(150);
}

function optimize(path: string): void {
  if (!existsSync(path)) return;
  try {
    execFileSync('oxipng', ['-o4', '--strip', 'safe', path], { stdio: 'ignore' });
  } catch {
    // Оптимизатор не установлен: PNG остаётся как есть.
  }
}

async function shoot(page: Page, name: string, options: Parameters<Page['screenshot']>[0] = {}): Promise<void> {
  await settle(page);
  const path = resolve(OUT, name);
  await page.screenshot({ path, animations: 'disabled', ...options });
  optimize(path);
}

test.use({ timezoneId: 'Europe/Moscow', browserName: 'chromium' });
test.beforeAll(() => mkdirSync(OUT, { recursive: true }));

// Скриншоты собирает только проект phone: остальные проекты Playwright пропускают файл.
test.beforeEach(({ browserName }, testInfo) => {
  test.skip(browserName !== 'chromium' || testInfo.project.name !== 'phone', 'скриншоты README собирает проект phone');
});

test.describe('скриншоты README для телефона', () => {
  test.use({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true
  });

  test('сессии в Аквамарине, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    seedReadmeSessions(api);
    await prepare(page, 'aquamarine', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-session]')).toHaveCount(4);
    await shoot(page, 'sessions-dark.png');
  });

  test('чат с кодом и инструментами в Аквамарине, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const id = seedChat(api);
    await prepare(page, 'aquamarine', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.prose pre code')).toBeVisible();
    await expect(page.locator('.tool .head')).toHaveCount(2);
    await page.locator('.tool .head').last().click();
    await expect(page.locator('.tool .out pre')).toContainText('test result: ok. 3 passed');
    await shoot(page, 'chat-dark.png');
  });

  test('карточка разрешения в VS Code, светлая', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const id = seedRequest(api, 'permission');
    await prepare(page, 'vscode', 'light');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${id}`);
    await expect(page.locator('.permission-card')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.permission-card .command-block')).toContainText('cargo test -p canterlot-hub');
    await shoot(page, 'permission-light.png');
  });

  test('карточка вопроса в Стандартной, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const id = seedRequest(api, 'question');
    await prepare(page, 'standard', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${id}`);
    await expect(page.locator('.question-card')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-question="q1"] [data-option="Аквамарин"]')).toBeVisible();
    await shoot(page, 'question-dark.png');
  });

  test('история лимитов в Стандартной, светлая', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedReadmeSessions(api);
    // На экране лимитов видны все четыре провайдера: у Antigravity два окна, у OpenCode ещё и месяц.
    api.limits.push(
      {
        provider: 'antigravity',
        weekly_reserve_active: false,
        five_hour: { used_permille: 180, resets_at: stamp(3 * 60 + 12), observed_at: stamp(-1) },
        weekly: { used_permille: 470, resets_at: stamp(5 * 24 * 60), observed_at: stamp(-1) }
      },
      {
        provider: 'opencode',
        weekly_reserve_active: false,
        data_confidence: 'estimated',
        five_hour: { used_permille: 90, resets_at: stamp(4 * 60), observed_at: stamp(-1) },
        weekly: { used_permille: 260, resets_at: stamp(6 * 24 * 60), observed_at: stamp(-1) },
        monthly: { used_permille: 120, resets_at: stamp(20 * 24 * 60), observed_at: stamp(-1) }
      }
    );
    await prepare(page, 'standard', 'light');
    await installChatRoutes(page, hub, api, OWNER);
    await seedSecondaryScreens(page, api, { machine: ids.machine, pinned: ids.themes }, null);
    await page.route('**/api/v1/limits/history?*', async (route) => {
      const url = new URL(route.request().url());
      const query = url.searchParams;
      const provider = query.get('provider') ?? 'claude';
      const zone = query.get('tz') ?? 'Europe/Moscow';
      const from = new Date(query.get('from') ?? stamp(-6 * 24 * 60));
      const days = Array.from({ length: 7 }, (_, index) => {
        const day = new Date(from);
        day.setDate(day.getDate() + index);
        return day;
      });
      const dayKey = (day: Date) => new Intl.DateTimeFormat('en-CA', {
        timeZone: zone,
        year: 'numeric',
        month: '2-digit',
        day: '2-digit'
      }).format(day);
      const speed = {
        claude: [{
          provider: 'claude',
          model: 'claude-opus-5-5',
          today: { runs: 4, tokens_per_second: 74.8, first_token_ms: 1900 },
          period: { runs: 38, tokens_per_second: 71.2, first_token_ms: 2100 }
        }],
        codex: [{
          provider: 'codex',
          model: 'gpt-6.1-sol',
          today: { runs: 3, tokens_per_second: 88.4, first_token_ms: 1300 },
          period: { runs: 27, tokens_per_second: 84.6, first_token_ms: 1400 }
        }],
        opencode: [{
          provider: 'opencode',
          model: 'deepseek',
          today: { runs: 2, tokens_per_second: 132.6, first_token_ms: 700 },
          period: { runs: 19, tokens_per_second: 126.4, first_token_ms: 800 }
        }]
      }[provider] ?? [];
      const series = provider === 'opencode'
        ? []
        : provider === 'claude'
          ? [{
              provider,
              five_hour: days.map((day, index) => ({
                observed_at: day.toISOString(),
                used_permille: 520 + index * 40,
                used_pct: (520 + index * 40) / 10
              })),
              weekly: days.map((day, index) => ({
                observed_at: day.toISOString(),
                used_permille: 710 + index * 22,
                used_pct: (710 + index * 22) / 10
              }))
            }]
          : [{
              provider,
              five_hour: [],
              weekly: days.map((day, index) => {
                // У Antigravity неделя сбросилась посреди периода: столбики снова высокие.
                const used = provider === 'antigravity' ? [620, 780, 910, 40, 210, 350, 470][index]! : 310 + index * 18;
                return { observed_at: day.toISOString(), used_permille: used, used_pct: used / 10 };
              })
            }];
      const usageRow = (patch: Record<string, unknown>) => ({
        day: dayKey(days.at(-1)!),
        provider,
        agent: 'main',
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        runs: 1,
        overlapped_runs: 0,
        ...patch
      });
      const usage = {
        claude: [
          usageRow({ conversation_id: ids.themes, model: 'opus-5.5', input_tokens: 126_000, output_tokens: 8_400, cache_write_tokens: 2_000, weekly_delta_permille: 32, runs: 3 }),
          usageRow({ conversation_id: ids.pinned, agent: 'agent-1', model: 'haiku', input_tokens: 18_000, output_tokens: 1_200, weekly_delta_permille: 2, overlapped_runs: 1 })
        ],
        codex: [usageRow({ conversation_id: ids.adapter, model: 'gpt-6.1-sol', input_tokens: 98_000, output_tokens: 6_700, cache_write_tokens: 1_400, weekly_delta_permille: 18, runs: 2 })],
        opencode: [usageRow({ conversation_id: ids.themes, model: 'deepseek', input_tokens: 84_000, output_tokens: 5_200, cache_write_tokens: 1_800, weekly_delta_permille: 12, runs: 2 })],
        antigravity: [usageRow({ conversation_id: ids.adapter, model: 'gemini-3-pro', input_tokens: 54_000, output_tokens: 3_100, cache_write_tokens: 900, weekly_delta_permille: 8 })]
      }[provider] ?? [];
      await route.fulfill({ json: { series, usage, speed } });
    });
    await page.goto('/limits');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('heading', { name: 'История лимитов' })).toBeVisible();
    await expect(page.getByRole('table', { name: 'Скорость моделей' })).toBeVisible();
    await expect(page.getByRole('table', { name: 'Скорость моделей' })).toContainText('deepseek');
    // Расход по сессиям показывает все четыре провайдера, а не только Claude и Codex.
    const usage = page.getByRole('table', { name: 'Сегодня по сессиям' });
    await expect(usage.locator('tbody tr')).toHaveCount(5);
    await expect(usage.locator('tbody tr').filter({ hasText: 'OpenCode · deepseek' })).toHaveCount(1);
    await expect(usage.locator('tbody tr').filter({ hasText: 'Antigravity · gemini-3-pro' })).toHaveCount(1);
    // Прокручиваем весь экран истории: в кадр попадают график, текущие лимиты, расход и скорость.
    await page.setViewportSize({ width: 390, height: Math.max(844, await page.locator('.secondary-body').evaluate((node) => node.scrollHeight + 120)) });
    await page.locator('.secondary-body').evaluate((node) => { node.scrollTop = 0; });
    await shoot(page, 'limits-light.png');
  });
});

test.describe('скриншот README для стола', () => {
  test.use({
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
    isMobile: false,
    hasTouch: false
  });

  test('список и чат рядом в Аквамарине, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedReadmeSessions(api);
    const id = seedChat(api, ids.themes);
    await prepare(page, 'aquamarine', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-layout="desktop"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="list"] [data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="chat"] [data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.tool .head')).toHaveCount(2);
    await page.locator('.tool .head').last().click();
    await shoot(page, 'desktop.png');
  });
});
