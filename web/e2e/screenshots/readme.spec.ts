import { test, expect, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi, leaseEntry, seedLease, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { Feed, hubText } from '../../testing/fixtures/feed-cards';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { FOUR, openViewer, seedFour, showChrome, thumb } from '../ui/image-gallery-fixture';
import { screenshotPng } from '../ui/image-viewer-fixture';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { PlanTask } from '../../src/lib/stores/plan.svelte';

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

function seedReadmeSessions(api: FakeHubApi): { pinned: string; themes: string; build: string; review: string; machine: string } {
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

  // Второе ожидание в разделе «Ждут вас»: строка списка начинается с состояния и возраста запроса.
  const build = seedConversation(api, {
    title: 'Сборка хаба',
    color: 'success',
    groups: [canterlot],
    model: 'opus-5.5',
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    machine: MACHINES.workstation,
    created_at: stamp(-34),
    last_activity_at: stamp(-12),
    activity: {
      state: 'waiting_for_user',
      evidence: { phase: 'awaiting_permission', request_kind: 'permission', request_tool: 'Bash', question: 'Bash: cargo test -p canterlot-hub' },
      since: stamp(-12)
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

  return { pinned, themes, build, review: reviewSession, machine: MACHINES.workstation.id };
}

function seedChat(api: FakeHubApi, existing?: string): string {
  seedMachines(api);
  seedLimits(api);
  const patch = {
    title: 'Темы оформления',
    color: 'blue',
    model: 'opus-5.5',
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    machine: MACHINES.workstation,
    created_at: stamp(-38),
    last_activity_at: stamp(-1),
    activity: { state: 'working' as const, evidence: { phase: 'generating', phase_since: stamp(-2) }, since: stamp(-2) }
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
  // Ждущее сообщение живёт только в очереди: полоса над полем ввода показывает текст и кнопку «Влить».
  const queued = { ...input(api, 'Добавь вариант «Норд» и проверь контраст в светлой теме.', '7', -1), state: 'queued' as const };
  api.histories.set(id, [user, answer, read, readResult, tests, testsResult, queued]);
  for (const block of [answer, read, readResult, tests, testsResult]) {
    block.agent = 'Claude Code';
    block.model = 'opus-5.5';
    api.blocks.set(block.id, { text: block.content, rev: 1, state: 'final' });
  }
  // Ход идёт, поэтому полоса очереди предлагает влить сообщение в него.
  const queue = { queued: 1, pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: user.id } } };
  for (const store of [api.list, api.details]) {
    const row = store.get(id);
    if (row) (row as { queue: unknown }).queue = queue;
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
  if (kind === 'permission') {
    // Перед карточкой разрешения видны шаги, которые к ней привели.
    const read = historyItem(api, run, {
      block_kind: 'tool_call',
      content: 'Прочитал 1 файл',
      tool_name: 'Read',
      summary: 'web/src/lib/theme.svelte.ts',
      created_seq: '3',
      created_at: stamp(-12)
    });
    const readResult = historyItem(api, run, {
      block_kind: 'tool_result',
      content: 'export function initTheme(): () => void {\n  // ...\n}',
      parent_block_id: read.id,
      created_seq: '4',
      created_at: stamp(-12)
    });
    const bash = historyItem(api, run, {
      block_kind: 'tool_call',
      content: '',
      tool_name: 'Bash',
      summary: 'cargo test -p canterlot-hub',
      created_seq: '5',
      created_at: stamp(-12)
    });
    api.histories.set(id, [user, answer, read, readResult, bash]);
    for (const block of [answer, read, readResult, bash]) {
      api.blocks.set(block.id, { text: block.content, rev: 1, state: 'final' });
    }
  } else {
    api.histories.set(id, [user]);
  }
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
              { label: 'VS Code', description: 'Как в редакторе, без сюрпризов' },
              { label: 'Норд', description: 'Холодные приглушённые тона' }
            ],
            multi_select: false,
            allow_custom: true
          }
        ]
      };

  api.hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    // Запрос ждёт 12 минут: возраст в шапке и на карточке показывает время ожидания.
    at: stamp(-12),
    payload: { request_id: api.id(), kind, payload }
  });
  api.hub.flush();
  return id;
}

const CLAUDE = { id: 'claude', title: 'Claude Code', provider: 'claude' };
const CODEX = { id: 'codex', title: 'Codex', provider: 'codex' };
const OPENCODE = { id: 'opencode', title: 'OpenCode', provider: 'opencode' };

interface OrchestratorSeed {
  orchestrator: string;
  executors: { gotify: string; watch: string; search: string; effort: string; board: string };
  machine: string;
}

/**
 * Оркестратор выпуска: доска плана во всех состояниях, делегаты на трёх харнессах
 * (один ждёт очереди на гейт), лента с карточками вызовов плана и запуска делегата.
 */
function seedOrchestrator(api: FakeHubApi): OrchestratorSeed {
  seedMachines(api);
  seedLimits(api);
  const machine = MACHINES.workstation;
  const orchestrator = seedConversation(api, {
    title: 'Выпуск 1.36',
    pinned: true,
    color: 'accent',
    model: 'opus-5.5',
    harness: CLAUDE,
    machine,
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
      created_at: stamp(created),
      last_activity_at: stamp(-2),
      activity: activity as never
    });
  const board = child('Доска плана', 'opus-5.5', CLAUDE, -130, { state: 'stopped', evidence: { kind: 'stopped', by: 'owner' }, since: stamp(-70) });
  const search = child('Поиск по истории', 'gpt-6.1-sol', CODEX, -110, { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-6) }, since: stamp(-6) });
  const effort = child('Смена effort на ходу', 'opus-5.5', CLAUDE, -95, { state: 'failed', evidence: { kind: 'failed', reason: 'process_exited' }, since: stamp(-24) });
  const gotify = child('Пуши Gotify', 'opus-5.5', CLAUDE, -48, { state: 'working', evidence: { phase: 'generating' }, since: stamp(-3) });
  const watch = child('Сторож машины', 'deepseek', OPENCODE, -30, {
    state: 'working',
    evidence: { phase: 'running_tool', tool_name: 'Bash', summary: 'scripts/check.sh --all', phase_since: stamp(-4), lease_wait: { kind: 'gate', position: 1, since: stamp(-4) } },
    since: stamp(-4)
  });

  // Отчёт исполнителя на проверке: ссылка «Отчёт» из плана ведёт на этот ответ.
  const report = historyItem(api, api.id(), {
    content: 'Поиск готов: индекс по ответам и сообщениям, подсветка совпадений, прыжок в ленту. Тесты зелёные.',
    agent: 'Codex',
    model: 'gpt-6.1-sol',
    created_seq: '1',
    created_at: stamp(-6)
  });
  api.histories.set(search, [report]);
  api.blocks.set(report.id, { text: report.content, rev: 1, state: 'final' });
  api.bindConversation(search);

  const tasks: PlanTask[] = [];
  const push = (task: Partial<PlanTask> & Pick<PlanTask, 'title'>) => {
    tasks.push(api.pushPlanTask({ orchestrator_id: orchestrator, created_at: stamp(-135), updated_at: stamp(-5), ...task }));
  };
  push({ title: 'Доска плана в хабе', state: 'done', executor_id: board, attempts: 1 });
  push({ title: 'Поиск по истории с подсветкой', state: 'in_review', executor_id: search, attempts: 1, findings: ['Прыжок к найденному ответу встаёт окном вокруг seq', 'Запрос не попадает в адрес страницы'] });
  push({ title: 'Пуши на телефон через Gotify', state: 'in_progress', executor_id: gotify, attempts: 1, note: 'Одно уведомление на событие, не больше 20 в минуту.' });
  push({ title: 'Смена effort посреди диалога', state: 'blocked', executor_id: effort, attempts: 2, blocker: 'CLI откатывает effort после отложенного перезапуска: нужен повтор на свежей версии' });
  push({ title: 'Сторож памяти и утёкших процессов', state: 'in_progress', executor_id: watch, attempts: 1 });
  push({ title: 'Скриншоты для README', state: 'waiting' });
  push({ title: 'Отдельный экран очереди', state: 'cancelled', note: 'Очередь живёт на экране машины.' });
  api.planDetails.set(tasks[1]!.id, {
    commits: [
      { sha: 'a41c9e07d2b3f6158e0a7c4d9b2e1f3a5c6d7e80', subject: 'Искать по ответам и сообщениям с морфологией' },
      { sha: 'b52dae18e3c4071690b18d5eac3f204b6d7e8f91', subject: 'Подсвечивать найденные слова в выдаче' },
      { sha: 'c63ebf29f4d5182701c29e6fbd40315c7e8f90a2', subject: 'Открывать ленту на месте совпадения' }
    ],
    commits_truncated: false,
    files_changed: 11,
    report_block_id: report.id,
    report_seq: 1
  });
  api.planDetails.set(tasks[2]!.id, {
    commits: [{ sha: 'd74fc03a05e6293812d3af70ce51426d8f90a1b3', subject: 'Добавить секцию [notify.gotify] в hub.toml' }],
    commits_truncated: false,
    files_changed: 4,
    report_block_id: null
  });

  const asOf = (task: PlanTask, patch: Partial<PlanTask>): PlanTask => ({ ...task, ...patch });
  const feed = new Feed(api, NOW_MS - 45 * 60_000);
  const run = api.id();
  feed.input('Разложи выпуск 1.36 на задачи и раздай делегатам. Гейт гоняй через очередь машины.');
  feed.text(run, 'Завожу план выпуска.');
  feed.call(run, 'add', 'mcp__canterlot__plan_add', { tasks: tasks.slice(1, 6).map((task) => ({ title: task.title })) }, hubText({
    numbers: [2, 3, 4, 5, 6],
    tasks: tasks.slice(1, 6).map((task) => asOf(task, { state: 'waiting', blocker: null, findings: [], executor_id: null, attempts: 0 }))
  }));
  feed.text(run, 'Запускаю делегата на пуши.');
  feed.call(run, 'spawn', 'mcp__canterlot__session_spawn', { title: 'Пуши Gotify', brief: 'Добавь пуши через Gotify: ожидание ответа, конец хода, сбой.', harness: 'claude', model: 'opus-5.5', effort: 'high', isolated: true, plan_task: 3 }, hubText({
    permission_mode: 'bypass', session_id: gotify, status: 'started', subscribed: true, plan_task: { number: 3, linked: true }, worktree: { branch: 'canterlot/7f3a91c2', path: '/srv/worktrees/7f3a91c2' }
  }, 'Session created. Linked to plan task #3: it moves to in_review when the session finishes.'));
  feed.call(run, 'blocked', 'mcp__canterlot__plan_update', { number: 4, blocker: tasks[3]!.blocker }, hubText({ status: 'updated', task: tasks[3] }));
  feed.call(run, 'review', 'mcp__canterlot__plan_update', { number: 2, state: 'in_review', findings: tasks[1]!.findings }, hubText({ status: 'updated', task: tasks[1] }));
  feed.text(run, 'Поиск ждёт проверки, смена effort заблокирована. Сторож машины стоит первым в очереди на гейт.');
  api.histories.set(orchestrator, feed.items);
  api.bindConversation(orchestrator);
  api.hub.reach(`conversation:${orchestrator}`, String(feed.seq));

  return { orchestrator, executors: { gotify, watch, search, effort, board }, machine: machine.id };
}

/** Сессии с совпадениями для поиска «гейт» разного происхождения: ответ, своё сообщение, итог делегата. */
function seedSearchHits(api: FakeHubApi, seed: OrchestratorSeed): void {
  const answer = (text: string, seq: number, at: number, agent: string, model: string): HistoryBlock => {
    const block = historyItem(api, api.id(), { content: text, agent, model, created_seq: String(seq), created_at: stamp(at) });
    api.blocks.set(block.id, { text, rev: 1, state: 'final' });
    return block;
  };
  const gotify = [
    input(api, 'Перед слиянием прогони полный гейт, но через очередь машины.', '1', -40),
    answer('Полный гейт встал в очередь вторым: машину держит сборка соседней сессии. Пока жду, дописываю тесты пушей.', 2, -36, 'Claude Code', 'opus-5.5'),
    answer('Гейт зелёный: fmt, clippy, nextest и Playwright прошли. Коммит готов к проверке.', 3, -12, 'Claude Code', 'opus-5.5')
  ];
  api.histories.set(seed.executors.gotify, gotify);
  api.bindConversation(seed.executors.gotify);
  const watch = [
    answer('Сторож видит две сборки вне очереди: они запущены мимо гейта и не делят слоты с остальными.', 1, -20, 'OpenCode', 'deepseek')
  ];
  api.histories.set(seed.executors.watch, watch);
  api.bindConversation(seed.executors.watch);
  const items = api.histories.get(seed.orchestrator) ?? [];
  const result: HistoryInput = {
    ...input(api, 'Итог: поиск готов, гейтом проверен на телефоне и столе.', String(items.length + 1), -5),
    origin: { kind: 'event', from: seed.executors.search, title: 'Поиск по истории', chain_depth: 1, event: { kind: 'session_result', state: 'idle', session_id: seed.executors.search, commits: [] } as never }
  } as HistoryInput;
  api.histories.set(seed.orchestrator, [...items, result]);
  api.hub.reach(`conversation:${seed.orchestrator}`, result.feed_seq!);
}

function seedMachineReport(api: FakeHubApi, seed: OrchestratorSeed): void {
  seedLease(api, seed.machine, [
    leaseEntry({ lease_id: 'h1', kind: 'build', conversation_id: seed.executors.gotify, since: stamp(-12) }),
    leaseEntry({ lease_id: 'h2', kind: 'build', label: 'check.sh --fast', dir: 'canterlot', since: stamp(-7) })
  ], [
    leaseEntry({ lease_id: 'w1', kind: 'gate', conversation_id: seed.executors.watch, requested_at: stamp(-4), position: 1 }),
    leaseEntry({ lease_id: 'w2', kind: 'build', label: 'npm run build', dir: 'canterlot-web', requested_at: stamp(-2), position: 2 })
  ]);
  const machine = api.machines.get(seed.machine)!;
  const HOUR = 60;
  const MIB = 1024 ** 2;
  machine.watch = {
    observed_at: stamp(0),
    hotspots: [
      { path: '/home/owner/src/canterlot/target', kind: 'target', bytes: 64_000_000_000, incremental_bytes: 22_000_000_000, newest_at: stamp(-2 * HOUR), measured_at: stamp(-1), conversation_id: null, title: null },
      { path: '/home/owner/.local/state/canterlot-worker/worktrees/019a6c1b-2d90-7e11-8c4f-a3b6e0d29174/target', kind: 'target', bytes: 9_400_000_000, incremental_bytes: 3_100_000_000, newest_at: stamp(-5 * HOUR), measured_at: stamp(-1), conversation_id: seed.executors.effort, title: 'Смена effort на ходу' },
      { path: '/home/owner/src/canterlot/web/node_modules', kind: 'node_modules', bytes: 1_100_000_000, incremental_bytes: 0, newest_at: stamp(-26 * HOUR), measured_at: stamp(-1), conversation_id: null, title: null }
    ],
    leaked: [
      { execution_id: '019a6c1b-2d90-7e11-8c4f-a3b6e0d29174', conversation_id: seed.executors.effort, title: 'Смена effort на ходу', pid: 48211, comm: 'cargo', rss_bytes: 610 * MIB, age_s: 3 * 3600 + 20 * 60 },
      { execution_id: '019a6c1b-2d90-7e11-8c4f-a3b6e0d29174', conversation_id: seed.executors.effort, title: 'Смена effort на ходу', pid: 48390, comm: 'cargo', rss_bytes: 370 * MIB, age_s: 3 * 3600 + 18 * 60 }
    ],
    unleased_count: 1
  } as never;
  machine.warnings = [...new Set([...machine.warnings, 'disk_usage_high', 'memory_low', 'processes_leaked'])];
  if (machine.metrics) machine.metrics = { ...machine.metrics, mem_available_bytes: Math.round(2.4 * 1024 ** 3) };
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

/** Экран второго уровня прокручивается к элементу: его край встаёт к краю контейнера. */
async function alignSecondary(page: Page, target: import('@playwright/test').Locator | string, edge: 'start' | 'end' = 'start'): Promise<void> {
  const body = page.locator('.secondary-body');
  const element = typeof target === 'string' ? body.locator(target) : target;
  const frame = (await body.boundingBox())!;
  const box = (await element.boundingBox())!;
  const current = await body.evaluate((node) => node.scrollTop);
  const top = box.y - frame.y + current;
  const next = edge === 'start' ? top - 8 : top + box.height - frame.height + 8;
  await body.evaluate((node, value) => { node.scrollTop = Math.max(0, value); }, next);
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
  test.use({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true
  });

  test('сессии в Аквамарине, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedReadmeSessions(api);
    await prepare(page, 'aquamarine', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-session]')).toHaveCount(4);
    // Раздел ожидания наверху, остаток на плашках лимитов и состояние в начале строки.
    await expect(page.getByRole('button', { name: 'Ждут вас · 2' })).toBeVisible();
    await expect(page.locator(`[data-waiting="${ids.build}"]`)).toContainText('Ждёт разрешения · Bash · 12 мин');
    await expect(page.locator('[data-limit-plaques]')).toContainText('ост.');
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
    // 1.36.0: очередь сообщений с кнопкой «Влить» и остаток контекста в поле ввода.
    await expect(page.locator('.strip').getByRole('button', { name: /^Влить: / })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Контекст: ост\./ })).toBeVisible();
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
    // Три провайдера: карточки текущих окон и начало истории помещаются в один кадр.
    api.limits.push(
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
    await seedSecondaryScreens(page, api, { machine: ids.machine, pinned: ids.themes });
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
        codex: [usageRow({ conversation_id: ids.pinned, model: 'gpt-6.1-sol', input_tokens: 98_000, output_tokens: 6_700, cache_write_tokens: 1_400, weekly_delta_permille: 18, runs: 2 })],
        opencode: [usageRow({ conversation_id: ids.themes, model: 'deepseek', input_tokens: 84_000, output_tokens: 5_200, cache_write_tokens: 1_800, weekly_delta_permille: 12, runs: 2 })],
        antigravity: [usageRow({ conversation_id: ids.pinned, model: 'gemini-3-pro', input_tokens: 54_000, output_tokens: 3_100, cache_write_tokens: 900, weekly_delta_permille: 8 })]
      }[provider] ?? [];
      await route.fulfill({ json: { series, usage, speed } });
    });
    await page.goto('/limits');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('heading', { name: 'История лимитов' })).toBeVisible();
    await expect(page.getByRole('table', { name: 'Скорость моделей' })).toBeVisible();
    await expect(page.getByRole('table', { name: 'Скорость моделей' })).toContainText('OpenCode');
    // Расход по сессиям показывает все четыре провайдера, а не только Claude и Codex.
    const usage = page.getByRole('table', { name: 'Сегодня по сессиям' });
    // Строки сессий идут под заголовками провайдеров: считаем только строки со ссылкой на сессию.
    await expect(usage.locator('tbody tr').filter({ has: page.getByRole('link') })).toHaveCount(5);
    await expect(usage.getByRole('rowheader', { name: 'OpenCode' })).toHaveCount(1);
    await expect(usage.getByRole('rowheader', { name: 'Antigravity' })).toHaveCount(1);
    await expect(usage.locator('tbody tr').filter({ hasText: 'gemini-3-pro' })).toHaveCount(1);
    // Кадр один и того же размера: сверху текущие окна с остатком, ниже — начало истории.
    await alignSecondary(page, '[data-region="now"]');
    await expect(page.getByRole('heading', { name: 'Сейчас' })).toBeInViewport({ ratio: 1 });
    await expect(page.getByRole('heading', { name: 'История лимитов' })).toBeInViewport({ ratio: 1 });
    // Часы закреплены: показания минутной давности не должны выглядеть устаревшими.
    await expect(page.locator('[data-region="now"]')).not.toContainText('устарело');
    await shoot(page, 'limits-light.png');
  });

  test('доска плана в Норде, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const seed = seedOrchestrator(api);
    await prepare(page, 'nord', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${seed.orchestrator}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    const strip = page.locator('[data-role="plan-strip"]');
    await expect(strip).toHaveText('План: 1 заблокирована · 1 на проверке');
    await strip.getByRole('button').click();
    const plan = page.locator('[data-role="plan-tab"]');
    await expect(plan.locator('[data-plan-group="in_progress"]')).toBeVisible();
    const task = plan.locator('[data-plan-task="2"]');
    await task.getByRole('button', { name: /#2 Поиск/ }).click();
    await expect(task.locator('[data-role="plan-task-body"]')).toContainText('3 коммита · 11 файлов изменено');
    await shoot(page, 'plan-dark.png');
  });

  test('карточки плана в ленте в Норде, светлая', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const seed = seedOrchestrator(api);
    await prepare(page, 'nord', 'light');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${seed.orchestrator}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.feed .feed-card').first()).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-role="plan-strip"]')).toBeVisible();
    await shoot(page, 'plan-feed-light.png');
  });

  test('панель делегирования в VS Code, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const seed = seedOrchestrator(api);
    await prepare(page, 'vscode', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${seed.orchestrator}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: /^Делегирование/ }).first().click();
    const sheet = page.getByRole('dialog', { name: 'Делегирование' });
    await expect(sheet).toBeVisible();
    await expect(sheet.locator(`a.tree-node[href$="/c/${seed.executors.watch}"]`)).toContainText('Ждёт очереди на гейт');
    await shoot(page, 'delegation-dark.png');
  });

  test('поиск по истории в Аквамарине, светлая', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const seed = seedOrchestrator(api);
    seedSearchHits(api, seed);
    await prepare(page, 'aquamarine', 'light');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: 'Поиск', exact: true }).click();
    await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('гейт');
    await page.locator('[data-deep-search]').click();
    await expect(page.locator('[data-search-view]')).toBeVisible();
    await expect(page.locator('[data-search-total]')).toHaveText('Найдено 7 совпадений');
    await expect(page.locator('[data-search-hit] mark').first()).toBeVisible();
    await page.locator('input:focus, textarea:focus').evaluateAll((nodes) => nodes.forEach((node) => (node as HTMLElement).blur()));
    await shoot(page, 'search-light.png');
  });

  test('просмотрщик картинок сессии в Стандартной, светлая', async ({ page }) => {
    await prepare(page, 'standard', 'light');
    // Пять картинок сессии, вторая открыта: счётчик «2 из 5» и кнопка «Показать в чате» в шапке.
    await seedFour(page);
    // Вторая картинка вертикальная, как снимок телефона: в кадре она занимает почти всю сцену.
    await page.route('**/api/v1/attachments/g-2/content', (route) => route.fulfill({ contentType: 'image/png', body: screenshotPng(900, 1400, 190) }));
    const dialog = await openViewer(page, thumb(page, FOUR.second));
    await expect(dialog.locator('.counter')).toHaveText('2 из 5');
    await expect(dialog.getByRole('button', { name: 'Показать в чате' })).toBeVisible();
    await settle(page);
    await showChrome(page);
    const path = resolve(OUT, 'gallery-light.png');
    await page.screenshot({ path, animations: 'disabled' });
    optimize(path);
  });

  test('новая сессия по прошлой в Стандартной, светлая', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedMockupSessions(api);
    api.machines.get(ids.machine)!.home_dir = '/srv';
    // Прошлая сессия задаёт машину, папку, модель и усилие: форма сворачивается в одну строку.
    seedConversation(api, {
      title: 'Скрипт выкатки',
      machine: { id: ids.machine, name: 'workstation' },
      harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
      workdir: '/srv/canterlot',
      model: 'opus',
      effort: 'medium',
      created_at: stamp(-2),
      last_activity_at: stamp(-2)
    });
    await prepare(page, 'standard', 'light');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/new');
    await expect(page.locator('[data-prefill]')).toHaveAttribute('data-prefill', 'done', { timeout: 15_000 });
    await expect(page.locator('[data-role="new-session-summary"]')).toHaveText('Как в прошлый раз · workstation · ~/canterlot · opus-5.5 · Обычное');
    await shoot(page, 'new-session-light.png');
  });

  test('очередь и сторож машины в Норде, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const seed = seedOrchestrator(api);
    await prepare(page, 'nord', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await seedSecondaryScreens(page, api, { machine: seed.machine, pinned: seed.orchestrator });
    seedMachineReport(api, seed);
    // Кадр 390×844 держит ресурсы, сторожа и очередь целиком: один кэш вместо каталога,
    // две сборки в работе и одна в ожидании вместо четырёх строк.
    const machine = api.machines.get(seed.machine)!;
    machine.metrics = { ...machine.metrics!, mem_available_bytes: Math.round(19.2 * 1024 ** 3), disks: [] };
    machine.warnings = [];
    machine.watch = {
      observed_at: stamp(0),
      hotspots: [{ path: '/home/owner/src/canterlot/target', kind: 'target', bytes: 64_000_000_000, incremental_bytes: 22_000_000_000, newest_at: stamp(-2 * 60), measured_at: stamp(-1), conversation_id: null, title: null }],
      leaked: [],
      unleased_count: 0
    } as never;
    seedLease(api, seed.machine, [
      leaseEntry({ lease_id: 'h1', kind: 'build', conversation_id: seed.executors.gotify, since: stamp(-12) }),
      leaseEntry({ lease_id: 'h2', kind: 'build', label: 'check.sh --fast', dir: 'canterlot', since: stamp(-7) })
    ], [
      leaseEntry({ lease_id: 'w1', kind: 'gate', conversation_id: seed.executors.watch, requested_at: stamp(-4), position: 1 })
    ]);
    await page.goto('/machines');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
    const main = page.locator('main');
    await expect(main.locator('[data-lease-queue] [data-lease-holder]')).toHaveCount(2);
    await expect(main.locator('[data-lease-queue] [data-lease-waiter]')).toHaveCount(1);
    await expect(main.getByRole('region', { name: 'Крупные кэши сборки' })).toBeVisible();
    // Ресурсы сверху, очередь внизу — оба края кадра на месте.
    const cpu = main.locator('section', { has: page.getByRole('heading', { name: 'CPU', exact: true }) }).first();
    await alignSecondary(page, cpu);
    await expect(cpu).toBeInViewport({ ratio: 1 });
    await expect(main.locator('[data-lease-queue]')).toBeInViewport({ ratio: 1 });
    await shoot(page, 'machines-dark.png');
  });
});

test.describe('скриншот README для стола', () => {
  test.use({
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
    isMobile: false,
    hasTouch: false
  });

  test('список, чат оркестратора и план рядом в Норде, тёмная', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    seedReadmeSessions(api);
    const seed = seedOrchestrator(api);
    api.counts = api.recount();
    await prepare(page, 'nord', 'dark');
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto(`/c/${seed.orchestrator}`);
    await expect(page.locator('[data-layout="desktop"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="list"] [data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-pane="chat"] [data-ready]')).toBeVisible({ timeout: 15_000 });
    // Свёрнутое дерево делегатов освобождает место: все группы списка видны целиком.
    await page.locator('[data-pane="list"]').getByRole('button', { name: /^Свернуть делегированные: 5$/ }).click();
    await expect(page.locator('[data-pane="list"]').getByRole('button', { name: /^Развернуть делегированные: 5$/ })).toBeVisible();
    await page.locator('[data-role="plan-strip"] button').click();
    const plan = page.locator('[data-pane="lane"] [data-role="plan-tab"]');
    await expect(plan.locator('[data-plan-group="in_progress"]')).toBeVisible();
    await plan.locator('[data-plan-task="2"]').getByRole('button', { name: /#2 Поиск/ }).click();
    await expect(plan.locator('[data-role="plan-task-body"]')).toContainText('3 коммита');
    await shoot(page, 'desktop.png');
  });
});
