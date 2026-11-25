import { test, expect, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { Feed, hubText } from '../../testing/fixtures/feed-cards';
import type { HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { PlanTask } from '../../src/lib/stores/plan.svelte';

// Снимки README. Все данные выдуманы: проект lighthouse (бэкенд интернет-магазина),
// машины workstation и mini, каталог /srv/lighthouse. Настоящих имён, адресов и путей здесь нет.
const OWNER = '00000000-0000-4000-8000-0000000000d1';
const OUT = resolve(import.meta.dirname, '../../../docs/screenshots');
// Часы страницы и заголовок X-Server-Time-Ms у FakeHub стоят на одном мгновении:
// иначе свежие лимиты выглядели бы устаревшими, а снимки менялись бы от прогона к прогону.
const NOW_MS = Date.parse('2026-11-15T15:00:00.000Z');
const NOW = new Date(NOW_MS).toISOString();

const MACHINES = {
  workstation: { id: '00000000-0000-4000-8000-000000000101', name: 'workstation' },
  mini: { id: '00000000-0000-4000-8000-000000000102', name: 'mini' }
} as const;

const OPUS = 'claude-opus-5-5';
const GPT = 'gpt-6.1-sol';
const DEEPSEEK = 'deepseek';
const WORKDIR = '/srv/lighthouse';

const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const CODEX = { id: 'codex', title: 'Codex', provider: 'codex' };
const OPENCODE = { id: 'opencode', title: 'OpenCode', provider: 'opencode' };

const HARNESSES = [
  { ...CLAUDE, models: [{ id: OPUS, title: 'Opus 5.5', efforts: ['low', 'medium', 'high'] }], permission_modes: ['owner', 'ask', 'bypass', 'read_only'] },
  { ...CODEX, models: [{ id: GPT, title: 'GPT-6.1 Sol', efforts: ['low', 'medium', 'high'] }], permission_modes: ['owner', 'ask', 'bypass', 'read_only'] },
  { ...OPENCODE, models: [{ id: DEEPSEEK, title: 'Deepseek', efforts: ['low', 'medium', 'high'] }], permission_modes: ['owner', 'ask', 'bypass', 'read_only'] }
];

function stamp(minutes: number): string {
  return new Date(NOW_MS + minutes * 60_000).toISOString();
}

function seedMachines(api: FakeHubApi): void {
  for (const machine of [MACHINES.workstation, MACHINES.mini]) {
    seedMachine(api, { id: machine.id, name: machine.name, availability: 'ready', os: 'linux', harnesses: HARNESSES });
  }
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

interface World {
  /** Идёт ход: экспорт заказов, закреплена. */
  exportCsv: string;
  /** Ждёт ответа на вопрос. */
  catalog: string;
  /** Ждёт разрешения на команду. */
  payments: string;
  /** Простаивает с превью последнего ответа. */
  review: string;
  machine: string;
}

/** Сессии владельца: раздел «Ждут вас», закреплённая, две группы и две машины. */
function seedWorld(api: FakeHubApi): World {
  seedMachines(api);
  seedLimits(api);

  const project = api.id();
  const reviews = api.id();
  api.groups = [
    { id: project, name: 'lighthouse', position: 0, icon: 'code', created_at: stamp(-7 * 24 * 60) },
    { id: reviews, name: 'Ревью', position: 1, icon: 'book', created_at: stamp(-7 * 24 * 60) }
  ];
  const common = { machine: MACHINES.workstation, workdir: WORKDIR };

  const exportCsv = seedConversation(api, {
    ...common,
    title: 'Экспорт заказов в CSV',
    pinned: true,
    color: 'accent',
    groups: [project],
    model: OPUS,
    harness: CLAUDE,
    created_at: stamp(-38),
    last_activity_at: stamp(-1),
    activity: { state: 'working', evidence: { phase: 'generating', phase_since: stamp(-2) }, since: stamp(-2) }
  });

  const catalog = seedConversation(api, {
    ...common,
    title: 'Кэш каталога',
    color: 'blue',
    groups: [project],
    unread_count: 2,
    model: DEEPSEEK,
    harness: OPENCODE,
    created_at: stamp(-90),
    last_activity_at: stamp(-6),
    activity: { state: 'waiting_for_user', evidence: { question: 'Какой TTL у кэша?' }, since: stamp(-7) }
  });

  const payments = seedConversation(api, {
    ...common,
    title: 'Миграция оплат',
    color: 'success',
    groups: [project],
    model: OPUS,
    harness: CLAUDE,
    created_at: stamp(-34),
    last_activity_at: stamp(-12),
    activity: {
      state: 'waiting_for_user',
      evidence: { phase: 'awaiting_permission', request_kind: 'permission', request_tool: 'Bash', question: 'Bash: sqlx migrate run' },
      since: stamp(-12)
    }
  });

  const review = seedConversation(api, {
    title: 'Ревью ветки поиска',
    color: 'neutral',
    groups: [reviews],
    model: GPT,
    harness: CODEX,
    machine: MACHINES.mini,
    workdir: WORKDIR,
    created_at: stamp(-180),
    last_activity_at: stamp(-27),
    preview: 'Два замечания по индексу',
    activity: { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-27) }, since: stamp(-27) }
  } as never);

  api.counts = api.recount();
  return { exportCsv, catalog, payments, review, machine: MACHINES.workstation.id };
}

const EXPORT_RS = `${WORKDIR}/src/orders/export.rs`;
const EXPORT_OLD = [
  'pub fn export_csv(orders: &[Order]) -> String {',
  '    todo!()',
  '}'
].join('\n');
const EXPORT_NEW = [
  'pub fn export_csv(orders: &[Order]) -> String {',
  '    let mut out = String::from("id,created_at,total\\n");',
  '    for order in orders {',
  '        let at = order.created_at.to_rfc3339();',
  '        out += &format!("{},{at},{}\\n", order.id, order.total);',
  '    }',
  '    out',
  '}'
].join('\n');
const EXPORT_TESTS = [
  'running 3 tests',
  'test orders::export::empty_list_has_only_header ... ok',
  'test orders::export::dates_are_rfc3339 ... ok',
  'test orders::export::total_is_in_kopecks ... ok',
  '',
  'test result: ok. 3 passed; 0 failed; finished in 0.04s'
].join('\n');

/** Лента сессии с кодом: ответ, правка файла, прогон тестов, сообщение в очереди и остаток контекста. */
function fillChat(api: FakeHubApi, id: string): void {
  const feed = new Feed(api, NOW_MS - 9 * 60_000);
  const run = api.id();
  feed.input('Добавь экспорт заказов в CSV, даты в ISO 8601.');
  const user = feed.items[0] as HistoryInput;
  feed.text(run, 'Экспорт живёт в `orders/export.rs`: заголовок и по строке на заказ.');
  feed.call(run, 'edit', 'Edit', { file_path: EXPORT_RS, old_string: EXPORT_OLD, new_string: EXPORT_NEW }, `The file ${EXPORT_RS} has been updated.`);
  feed.call(run, 'tests', 'Bash', { command: 'cargo test -p lighthouse orders::export', description: 'Тесты экспорта заказов' }, EXPORT_TESTS, {}, { summary: 'exit 0' });
  feed.text(run, 'Три теста зелёные. Проверить руками:\n\n```bash\ncurl -O /api/orders.csv\n```\n\nОсталось описать маршрут в документации.');
  // Ждущее сообщение живёт только в очереди: полоса над полем ввода показывает текст и кнопку «Влить».
  feed.input('Ещё добавь фильтр по статусу заказа.', { state: 'queued' } as Partial<HistoryInput>);
  api.histories.set(id, feed.items);
  // Ход идёт, поэтому полоса очереди предлагает влить сообщение в него.
  const queue = { queued: 1, pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: user.id } } };
  for (const store of [api.list, api.details]) {
    const row = store.get(id);
    if (row) (row as { queue: unknown }).queue = queue;
  }
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  api.pushContext(id, 127_600, 200_000);
  api.hub.flush();
}

/** Сессия ждёт разрешения на команду: перед карточкой видны шаги, которые к ней привели. */
function fillPermission(api: FakeHubApi, id: string): void {
  const migration = `${WORKDIR}/migrations/0042_payments_split.sql`;
  const feed = new Feed(api, NOW_MS - 15 * 60_000);
  const run = api.id();
  feed.input('Раздели таблицу оплат на оплаты и возвраты. Сначала проверь миграцию на тестовой базе.');
  feed.text(run, 'Миграция готова: возвраты уезжают в свою таблицу, суммы сверяются до и после. Прогоню её на тестовой базе.');
  feed.call(run, 'read', 'Read', { file_path: migration }, '1\tcreate table refunds (');
  feed.call(run, 'migrate', 'Bash', { command: 'sqlx migrate run -D $TEST_DB', description: 'Применить миграцию на тестовой базе' }, null);
  api.histories.set(id, feed.items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  api.hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    // Запрос ждёт 12 минут: возраст в шапке и на карточке показывает время ожидания.
    at: stamp(-12),
    payload: {
      request_id: api.id(),
      kind: 'permission',
      payload: {
        tool_name: 'Bash',
        description: 'Применить миграцию на тестовой базе',
        command: 'sqlx migrate run -D $TEST_DB',
        cwd: WORKDIR
      }
    }
  });
  api.hub.flush();
}

interface OrchestratorSeed {
  orchestrator: string;
  executors: { mail: string; load: string; search: string; payments: string; exportCsv: string };
}

/**
 * Оркестратор выпуска: доска плана во всех состояниях, делегаты на трёх харнессах
 * (один ждёт очереди на гейт), лента с карточками вызовов плана и запуска делегата.
 */
function seedOrchestrator(api: FakeHubApi): OrchestratorSeed {
  const machine = MACHINES.workstation;
  const orchestrator = seedConversation(api, {
    title: 'Выпуск 2.4',
    pinned: true,
    color: 'accent',
    model: OPUS,
    harness: CLAUDE,
    machine,
    workdir: WORKDIR,
    created_at: stamp(-140),
    last_activity_at: stamp(-1),
    activity: { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-1) }, since: stamp(-1) } as never
  });
  const child = (title: string, model: string, harness: typeof CLAUDE, created: number, activity: Record<string, unknown>) =>
    seedConversation(api, {
      title,
      parent_id: orchestrator,
      spawn_depth: 1,
      harness,
      model,
      machine,
      workdir: WORKDIR,
      created_at: stamp(created),
      last_activity_at: stamp(-2),
      activity: activity as never
    });
  const exportCsv = child('Экспорт заказов', OPUS, CLAUDE, -130, { state: 'stopped', evidence: { kind: 'stopped', by: 'owner' }, since: stamp(-70) });
  const search = child('Поиск с опечатками', GPT, CODEX, -110, { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-6) }, since: stamp(-6) });
  const payments = child('Перенос оплат', OPUS, CLAUDE, -95, { state: 'failed', evidence: { kind: 'failed', reason: 'crashed' }, since: stamp(-24) });
  const mail = child('Письма о заказе', OPUS, CLAUDE, -48, { state: 'working', evidence: { phase: 'generating' }, since: stamp(-3) });
  const load = child('Нагрузка на корзину', DEEPSEEK, OPENCODE, -30, {
    state: 'working',
    evidence: { phase: 'running_tool', tool_name: 'Bash', summary: 'scripts/check.sh --all', phase_since: stamp(-4), lease_wait: { kind: 'gate', position: 1, since: stamp(-4) } },
    since: stamp(-4)
  });

  // Отчёт исполнителя на проверке: ссылка «Отчёт» из плана ведёт на этот ответ.
  const reportFeed = new Feed(api, NOW_MS - 7 * 60_000);
  const report = reportFeed.block({ content: 'Поиск готов: триграммный индекс, одна опечатка на слово, подсветка совпадения. Тесты зелёные.', agent: 'Codex', model: GPT });
  api.histories.set(search, reportFeed.items);
  api.bindConversation(search);

  const tasks: PlanTask[] = [];
  const push = (task: Partial<PlanTask> & Pick<PlanTask, 'title'>) => {
    tasks.push(api.pushPlanTask({ orchestrator_id: orchestrator, created_at: stamp(-135), updated_at: stamp(-5), ...task }));
  };
  push({ title: 'Экспорт заказов в CSV', state: 'done', executor_id: exportCsv, attempts: 1 });
  push({ title: 'Поиск по каталогу с опечатками', state: 'in_review', executor_id: search, attempts: 1, findings: ['Триграммный индекс вместо полного перебора', 'Запрос короче трёх букв ищется по началу слова'] });
  push({ title: 'Письма о статусе заказа', state: 'in_progress', executor_id: mail, attempts: 1, note: 'Одно письмо на смену статуса, без повторов.' });
  push({ title: 'Перенос оплат на новую схему', state: 'blocked', executor_id: payments, attempts: 2, blocker: 'Нужен свежий дамп базы: без него миграцию не проверить' });
  push({ title: 'Нагрузочный тест корзины', state: 'in_progress', executor_id: load, attempts: 1 });
  push({ title: 'Документация API', state: 'waiting' });
  push({ title: 'Отдельный сервис уведомлений', state: 'cancelled', note: 'Хватает очереди в основном сервисе.' });
  api.planDetails.set(tasks[1]!.id, {
    commits: [
      { sha: 'a41c9e07d2b3f6158e0a7c4d9b2e1f3a5c6d7e80', subject: 'Искать товары по триграммам' },
      { sha: 'b52dae18e3c4071690b18d5eac3f204b6d7e8f91', subject: 'Терпеть одну опечатку в слове' },
      { sha: 'c63ebf29f4d5182701c29e6fbd40315c7e8f90a2', subject: 'Подсвечивать совпадение в названии' }
    ],
    commits_truncated: false,
    files_changed: 11,
    report_block_id: report.id,
    report_seq: 1
  });
  api.planDetails.set(tasks[2]!.id, {
    commits: [{ sha: 'd74fc03a05e6293812d3af70ce51426d8f90a1b3', subject: 'Слать письмо при смене статуса заказа' }],
    commits_truncated: false,
    files_changed: 4,
    report_block_id: null
  });

  const asOf = (task: PlanTask, patch: Partial<PlanTask>): PlanTask => ({ ...task, ...patch });
  const feed = new Feed(api, NOW_MS - 45 * 60_000);
  const run = api.id();
  feed.input('Разложи выпуск 2.4 на задачи и раздай делегатам. Полную проверку гоняй через очередь машины.');
  feed.text(run, 'Завожу план выпуска.');
  feed.call(run, 'add', 'mcp__canterlot__plan_add', { tasks: tasks.slice(1, 6).map((task) => ({ title: task.title })) }, hubText({
    numbers: [2, 3, 4, 5, 6],
    tasks: tasks.slice(1, 6).map((task) => asOf(task, { state: 'waiting', blocker: null, findings: [], executor_id: null, attempts: 0 }))
  }));
  feed.text(run, 'Запускаю делегата на письма.');
  feed.call(run, 'spawn', 'mcp__canterlot__session_spawn', { title: 'Письма о заказе', brief: 'Письмо покупателю при смене статуса заказа: шаблон, очередь, тест.', harness: 'claude_code', model: OPUS, effort: 'high', isolated: true, plan_task: 3 }, hubText({
    permission_mode: 'bypass', session_id: mail, status: 'started', subscribed: true, plan_task: { number: 3, linked: true }, worktree: { branch: 'canterlot/7f3a91c2', path: '/srv/worktrees/7f3a91c2' }
  }, 'Session created. Linked to plan task #3: it moves to in_review when the session finishes.'));
  feed.call(run, 'blocked', 'mcp__canterlot__plan_update', { number: 4, blocker: tasks[3]!.blocker }, hubText({ status: 'updated', task: tasks[3] }));
  feed.call(run, 'review', 'mcp__canterlot__plan_update', { number: 2, state: 'in_review', findings: tasks[1]!.findings }, hubText({ status: 'updated', task: tasks[1] }));
  feed.text(run, 'Поиск ждёт проверки, перенос оплат заблокирован. Нагрузочный тест стоит первым в очереди на гейт.');
  api.histories.set(orchestrator, feed.items);
  api.bindConversation(orchestrator);
  api.hub.reach(`conversation:${orchestrator}`, String(feed.seq));
  api.counts = api.recount();

  return { orchestrator, executors: { mail, load, search, payments, exportCsv } };
}

/** Третий провайдер и история за неделю: экран лимитов показывает окна, скорость и расход по сессиям. */
async function seedLimitsHistory(page: Page, api: FakeHubApi, world: World): Promise<void> {
  api.limits.push({
    provider: 'opencode',
    weekly_reserve_active: false,
    data_confidence: 'estimated',
    five_hour: { used_permille: 90, resets_at: stamp(4 * 60), observed_at: stamp(-1) },
    weekly: { used_permille: 260, resets_at: stamp(6 * 24 * 60), observed_at: stamp(-1) },
    monthly: { used_permille: 120, resets_at: stamp(20 * 24 * 60), observed_at: stamp(-1) }
  });
  await page.route('**/api/v1/limits/history?*', async (route) => {
    const query = new URL(route.request().url()).searchParams;
    const provider = query.get('provider') ?? 'claude';
    const zone = query.get('tz') ?? 'Europe/Moscow';
    const from = new Date(query.get('from') ?? stamp(-6 * 24 * 60));
    const days = Array.from({ length: 7 }, (_, index) => {
      const day = new Date(from);
      day.setDate(day.getDate() + index);
      return day;
    });
    const dayKey = (day: Date) => new Intl.DateTimeFormat('en-CA', { timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit' }).format(day);
    const rate = (model: string, today: number, period: number, first: number) => [{
      provider,
      model,
      today: { runs: 4, tokens_per_second: today, first_token_ms: first },
      period: { runs: 38, tokens_per_second: period, first_token_ms: first + 150 }
    }];
    const speed = { claude: rate(OPUS, 74.8, 71.2, 1900), codex: rate(GPT, 88.4, 84.6, 1300), opencode: rate(DEEPSEEK, 132.6, 126.4, 700) }[provider] ?? [];
    const points = (base: number, step: number) => days.map((day, index) => {
      const used = base + index * step;
      return { observed_at: day.toISOString(), used_permille: used, used_pct: used / 10 };
    });
    const series = provider === 'opencode'
      ? []
      : provider === 'claude'
        ? [{ provider, five_hour: points(520, 40), weekly: points(710, 22) }]
        : [{ provider, five_hour: [], weekly: points(310, 18) }];
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
        usageRow({ conversation_id: world.exportCsv, model: OPUS, input_tokens: 126_000, output_tokens: 8_400, cache_write_tokens: 2_000, weekly_delta_permille: 32, runs: 3 }),
        usageRow({ conversation_id: world.payments, model: OPUS, input_tokens: 18_000, output_tokens: 1_200, weekly_delta_permille: 2 })
      ],
      codex: [usageRow({ conversation_id: world.review, model: GPT, input_tokens: 98_000, output_tokens: 6_700, cache_write_tokens: 1_400, weekly_delta_permille: 18, runs: 2 })],
      opencode: [usageRow({ conversation_id: world.catalog, model: DEEPSEEK, input_tokens: 84_000, output_tokens: 5_200, cache_write_tokens: 1_800, weekly_delta_permille: 12, runs: 2 })]
    }[provider] ?? [];
    await route.fulfill({ json: { series, usage, speed } });
  });
}

async function prepare(page: Page, palette: 'aquamarine' | 'standard' | 'vscode' | 'nord', theme: 'dark' | 'light'): Promise<void> {
  await page.addInitScript(({ palette, theme }) => {
    localStorage.setItem('canterlot.palette', palette);
    localStorage.setItem('canterlot.theme', theme);
    localStorage.setItem('canterlot.motion', 'off');
  }, { palette, theme });
  // Дата закреплена, а таймеры и анимации идут как обычно: иначе лимиты уезжали бы к настоящему «сейчас».
  await page.clock.setFixedTime(new Date(NOW));
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

/** Экран второго уровня прокручивается к элементу: его верх встаёт к верху контейнера. */
async function alignSecondary(page: Page, selector: string): Promise<void> {
  const body = page.locator('.secondary-body');
  const frame = (await body.boundingBox())!;
  const box = (await body.locator(selector).boundingBox())!;
  const current = await body.evaluate((node) => node.scrollTop);
  await body.evaluate((node, value) => { node.scrollTop = Math.max(0, value); }, box.y - frame.y + current - 8);
}

async function shoot(page: Page, name: string): Promise<void> {
  await settle(page);
  const path = resolve(OUT, name);
  await page.screenshot({ path, animations: 'disabled' });
  optimize(path);
}

async function boot(page: Page, palette: Parameters<typeof prepare>[1], theme: Parameters<typeof prepare>[2]): Promise<{ hub: FakeHub; api: FakeHubApi; world: World }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const world = seedWorld(api);
  await prepare(page, palette, theme);
  await installChatRoutes(page, hub, api, OWNER);
  return { hub, api, world };
}

test.use({ timezoneId: 'Europe/Moscow', browserName: 'chromium' });
test.beforeAll(() => mkdirSync(OUT, { recursive: true }));

// Скриншоты собирает только проект phone: остальные проекты Playwright пропускают файл.
let restoreServerClock: (() => void) | null = null;
test.beforeEach(({ browserName }, testInfo) => {
  test.skip(browserName !== 'chromium' || testInfo.project.name !== 'phone', 'скриншоты README собирает проект phone');
  // Заголовок времени FakeHub читает Date.now процесса: без этой подмены «сейчас» уезжало бы с каждым прогоном.
  const real = Date.now;
  Date.now = () => NOW_MS;
  restoreServerClock = () => { Date.now = real; };
});
test.afterEach(() => {
  restoreServerClock?.();
  restoreServerClock = null;
});

test.describe('скриншоты README для телефона', () => {
  // Все кадры телефона одного размера: 390×844 при плотности 2.
  test.use({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true
  });

  test('сессии в Аквамарине, тёмная', async ({ page }) => {
    const { world } = await boot(page, 'aquamarine', 'dark');
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-session]')).toHaveCount(4);
    // Раздел ожидания наверху, остаток на плашках лимитов и состояние в начале строки.
    await expect(page.getByRole('button', { name: 'Ждут вас · 2' })).toBeVisible();
    await expect(page.locator(`[data-waiting="${world.payments}"]`)).toContainText('Ждёт разрешения · Bash · 12 мин');
    await expect(page.locator('[data-limit-plaques]')).toContainText('ост.');
    await shoot(page, 'sessions-dark.png');
  });

  test('чат с кодом и инструментами в Стандартной, светлая', async ({ page }) => {
    const { api, world } = await boot(page, 'standard', 'light');
    fillChat(api, world.exportCsv);
    await page.goto(`/c/${world.exportCsv}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.prose pre code')).toBeVisible();
    // Очередь сообщений с кнопкой «Влить» и остаток контекста в поле ввода.
    await expect(page.locator('.strip').getByRole('button', { name: /^Влить: / })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Контекст: ост\./ })).toBeVisible();
    await shoot(page, 'chat-light.png');
  });

  test('карточка разрешения в VS Code, тёмная', async ({ page }) => {
    const { api, world } = await boot(page, 'vscode', 'dark');
    fillPermission(api, world.payments);
    await page.goto(`/c/${world.payments}`);
    await expect(page.locator('.permission-card')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.permission-card .command-block')).toContainText('sqlx migrate run');
    await shoot(page, 'permission-dark.png');
  });

  test('делегаты и план в Норде, тёмная', async ({ page }) => {
    const { api } = await boot(page, 'nord', 'dark');
    const seed = seedOrchestrator(api);
    await page.goto(`/c/${seed.orchestrator}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: /^Делегирование/ }).first().click();
    const sheet = page.getByRole('dialog', { name: /^Делегирование/ });
    await expect(sheet).toBeVisible();
    await expect(sheet.locator(`a.tree-node[href$="/c/${seed.executors.load}"]`)).toContainText('Ждёт очереди на гейт');
    await shoot(page, 'delegation-dark.png');
  });

  test('лимиты в Аквамарине, светлая', async ({ page }) => {
    const { api, world } = await boot(page, 'aquamarine', 'light');
    await seedSecondaryScreens(page, api, { machine: world.machine, pinned: world.exportCsv });
    // Свой ответ истории ставится после общих маршрутов экрана: Playwright берёт последний.
    await seedLimitsHistory(page, api, world);
    await page.goto('/limits');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('heading', { name: 'История лимитов' })).toBeVisible();
    // Сверху текущие окна с остатком, ниже начало истории.
    await alignSecondary(page, '[data-region="now"]');
    await expect(page.getByRole('heading', { name: 'Сейчас' })).toBeInViewport({ ratio: 1 });
    // Часы закреплены: показания минутной давности не должны выглядеть устаревшими.
    await expect(page.locator('[data-region="now"]')).not.toContainText('устарело');
    await shoot(page, 'limits-light.png');
  });
});

test.describe('скриншоты README для стола', () => {
  // От 1600 px ширины панель плана и делегатов открывается сама; плотность 2 держит текст чётким.
  test.use({
    viewport: { width: 1600, height: 1000 },
    deviceScaleFactor: 2,
    isMobile: false,
    hasTouch: false
  });

  test('список, чат оркестратора и панель плана в Норде, тёмная', async ({ page }) => {
    const { api } = await boot(page, 'nord', 'dark');
    const seed = seedOrchestrator(api);
    await page.goto(`/c/${seed.orchestrator}`);
    await expect(page.locator('[data-layout="desktop"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="list"] [data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="chat"] [data-ready]')).toBeVisible({ timeout: 15_000 });
    // Правая панель открыта без нажатий: план и делегаты стоят стопкой.
    const lane = page.locator('[data-pane="lane"]');
    await expect(lane.locator('[data-plan-group="in_progress"]')).toBeVisible();
    await shoot(page, 'desktop-dark.png');
  });

  test('список и чат с кодом в Стандартной, светлая', async ({ page }) => {
    const { api, world } = await boot(page, 'standard', 'light');
    seedOrchestrator(api);
    fillChat(api, world.exportCsv);
    await page.goto(`/c/${world.exportCsv}`);
    await expect(page.locator('[data-layout="desktop"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="list"] [data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="chat"] [data-ready]')).toBeVisible({ timeout: 15_000 });
    const chat = page.locator('[data-pane="chat"]');
    await expect(chat.locator('.prose pre code')).toBeVisible();
    // Правка раскрыта: в ленте видна разница файла.
    await chat.getByRole('button', { name: /Правка/ }).first().click();
    await expect(chat.getByText('to_rfc3339()')).toBeVisible();
    await shoot(page, 'desktop-light.png');
  });
});
