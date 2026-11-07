// Лента оркестратора со всеми карточками инструментов: e2e, экран visual-language и видео на ревью.
import { createHash } from 'node:crypto';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { FakeHub } from '../fake-hub/hub';
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';
import { seedRoutingCatalog } from './routing';

const MINUTE = 60_000;
const MODEL = 'claude-opus-5-5';

/** Ответ хаба, как его печатает воркер: JSON с отступами и подсказка отдельной строкой. */
export function hubText(value: unknown, hint = ''): string {
  return JSON.stringify(value, null, 2) + (hint ? `\n${hint}` : '');
}

export interface FeedCardsSeed {
  id: string;
  /** Делегат, запущенный в начале ленты: его состояние живое. */
  child: string;
  routed: string;
  second: string;
  accepted: string;
  run: string;
  /** id блоков по имени сцены. */
  blocks: Record<string, string>;
  /** Картинка, переданная владельцу. */
  imageId: string;
}

class Feed {
  seq = 0;
  readonly items: (HistoryBlock | HistoryInput)[] = [];
  readonly blocks: Record<string, string> = {};
  constructor(
    readonly api: FakeHubApi,
    public at: number
  ) {}

  stamp(step = 20_000): string {
    this.at += step;
    return new Date(this.at).toISOString();
  }

  input(text: string, extra: Partial<HistoryInput> = {}): void {
    const seq = String(++this.seq);
    this.items.push({ kind: 'input', id: this.api.id(), idempotency_key: `cards-${seq}`, state: 'accepted', content: [{ type: 'text', text }], attachments: [], created_seq: seq, feed_seq: seq, created_at: this.stamp(), ...extra } as HistoryInput);
  }

  block(partial: Partial<HistoryBlock>): HistoryBlock {
    const id = partial.id ?? this.api.id();
    const content = partial.content ?? '';
    const at = this.stamp(4_000);
    this.api.blocks.set(id, { text: content, rev: 1, state: 'final' });
    const block = {
      kind: 'block',
      id,
      block_kind: 'text',
      content,
      content_bytes: new TextEncoder().encode(content).length,
      content_truncated: false,
      created_seq: String(++this.seq),
      created_at: at,
      started_at: at,
      finished_at: at,
      rev: 1,
      state: 'final',
      agent: 'orchestrator',
      model: MODEL,
      tool_name: null,
      summary: null,
      parent_block_id: null,
      input_command_id: null,
      line_count: content.split('\n').length,
      ...partial
    } as HistoryBlock;
    this.items.push(block);
    return block;
  }

  text(run: string, content: string): void {
    this.block({ content, input_command_id: run });
  }

  call(run: string, name: string, tool: string, input: unknown, result: string | null, patch: Partial<HistoryBlock> = {}, resultPatch: Partial<HistoryBlock> = {}): string {
    const content = typeof input === 'string' ? input : JSON.stringify(input);
    const call = this.block({ block_kind: 'tool_call', tool_name: tool, summary: tool, content, input_command_id: run, ...patch });
    this.blocks[name] = call.id;
    if (result !== null) this.block({ block_kind: 'tool_result', parent_block_id: call.id, input_command_id: run, content: result, ...resultPatch });
    return call.id;
  }
}

const BRIEF = [
  'Собери карточки ленты для вызовов инструментов: реестр, нормализатор имён, тесты.',
  '',
  'Карточки: запуск делегата, приёмка, сообщение, задачи, будильники, файл, лимиты.',
  'Сводка каждой — одна строка; сырой вызов раскрывается.',
  'Не трогай MediaViewer.svelte: его переписывает другая сессия.'
].join('\n');

const TODO_FIRST = {
  todos: [
    { content: 'Нормализатор имён инструментов', status: 'completed', activeForm: 'Пишу нормализатор' },
    { content: 'Реестр карточек', status: 'in_progress', activeForm: 'Пишу реестр' },
    { content: 'Карточка запуска делегата', status: 'pending', activeForm: 'Пишу карточку запуска' },
    { content: 'Тесты и видео', status: 'pending', activeForm: 'Пишу тесты' }
  ]
};

export const TODO_SECOND = {
  todos: [
    { content: 'Нормализатор имён инструментов', status: 'completed', activeForm: 'Пишу нормализатор' },
    { content: 'Реестр карточек', status: 'completed', activeForm: 'Пишу реестр' },
    { content: 'Карточка запуска делегата', status: 'in_progress', activeForm: 'Пишу карточку запуска' },
    { content: 'Тесты и видео', status: 'pending', activeForm: 'Пишу тесты' },
    { content: 'Скриншоты для владельца', status: 'pending', activeForm: 'Снимаю скриншоты' }
  ]
};

export const IMAGE_ID = '00000000-0000-4000-8000-00000000ca01';

/** Лента оркестратора: каждая карточка реестра, обычный блок Bash и незнакомый инструмент. */
export function seedFeedCards(api: FakeHubApi, opts: { now?: number } = {}): FeedCardsSeed {
  const now = opts.now ?? Date.now();
  const id = seedConversation(api, { title: 'Кантерлот', model: MODEL, harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' } });
  const child = seedConversation(api, { title: 'Карточки ленты', parent_id: id, spawn_depth: 1, model: 'opus[1m]', activity: { state: 'working', evidence: { phase: 'running_tool' }, since: new Date(now).toISOString() } });
  const routed = seedConversation(api, { title: 'Фикс скролла', parent_id: id, spawn_depth: 1, model: 'opencode-go/deepseek-v4.1-flash', activity: { state: 'waiting_for_user', evidence: { question: 'Какой порог?' }, since: new Date(now).toISOString() } });
  const second = seedConversation(api, { title: 'Ревью вёрстки', parent_id: id, spawn_depth: 1, model: 'gpt-6.1-sol', activity: { state: 'idle', evidence: { last_turn_at: new Date(now).toISOString() }, since: new Date(now).toISOString() } });
  const accepted = seedConversation(api, { title: 'Миграция лимитов', parent_id: id, spawn_depth: 1, accepted: true, archived: true, activity: { state: 'stopped', evidence: {}, since: new Date(now).toISOString() } });
  seedRoutingCatalog(api);
  const nextSlot = new Date(now + 25 * MINUTE);
  const slotUtc = `${String(nextSlot.getUTCHours()).padStart(2, '0')}:${String(nextSlot.getUTCMinutes()).padStart(2, '0')}`;
  const feed = new Feed(api, now - 50 * MINUTE);
  const run = api.id();

  feed.input('Собери карточки инструментов в ленте и раздай работу делегатам');
  feed.text(run, 'Начинаю с плана и лимитов.');
  feed.call(run, 'todo1', 'TodoWrite', TODO_FIRST, 'Todos have been modified successfully. Ensure that you continue to use the todo list to track your progress.');
  feed.call(run, 'toolsearch', 'ToolSearch', { query: 'select:mcp__canterlot__session_spawn', max_results: 1 }, 'Tool loaded.');
  feed.call(run, 'monitor', 'Monitor', { description: 'ждёт сборку web', command: 'until test -f /tmp/build.ok; do sleep 2; done' }, 'Monitoring started.');
  feed.text(run, 'Лимиты перед долгой работой:');
  feed.call(run, 'limits', 'mcp__canterlot__canterlot_limits', {}, 'Claude API budget: five-hour window 15% used, resets 2026-11-07 11:20 UTC; weekly window 39% used, resets 2026-11-10 01:00 UTC.\nCodex API budget: five-hour window 4.1% used; weekly window 83% used, resets 2026-11-10 08:00 UTC.\nOpenCode API budget: five-hour window 97% used, resets 2026-11-07 12:00 UTC.\nObserved: claude just now, codex 29 min ago, opencode 2 h ago.');
  feed.text(run, 'Запускаю делегатов.');
  feed.call(run, 'spawn', 'mcp__canterlot__session_spawn', { title: 'Карточки ленты', brief: BRIEF, harness: 'claude_code', model: 'opus[1m]', effort: 'high', isolated: true }, hubText({ permission_mode: 'bypass', session_id: child, status: 'started', subscribed: true, worktree: { branch: 'canterlot/8b993844', path: '/data/worktrees/8b993844' } }, 'Session created. You are subscribed to its completion; the summary arrives in your queue. Do not poll.'));
  feed.call(run, 'spawnRouted', 'mcp__canterlot__session_spawn', { title: 'Фикс скролла', brief: 'Поправь якорь прокрутки в ленте.', route: { task_type: 'bugfix' } }, hubText({ permission_mode: 'bypass', routing: { effort: 'high', harness: 'opencode', model: 'opencode-go/deepseek-v4.1-flash', policy_revision: 0, task_type: 'bugfix' }, session_id: routed, status: 'started', subscribed: true }, 'Routed bugfix to opencode/opencode-go/deepseek-v4.1-flash (high);'));
  feed.text(run, 'Ещё два запуска: один вне белого списка, второй упёрся в частоту.');
  feed.call(run, 'spawnPending', 'mcp__canterlot__session_spawn', { title: 'Эксперимент с Gemini', brief: 'Сравни рендер карточек в Antigravity.', harness: 'antigravity', model: 'gemini-3-pro', effort: 'high' }, hubText({ request_id: api.id(), status: 'pending_approval' }, "This harness or model is outside the owner's list. The owner decides in the app; the outcome arrives as a message."));
  feed.call(run, 'spawnQueued', 'mcp__canterlot__session_spawn', { title: 'Скриншоты тем', brief: 'Сними 15 палитр.', harness: 'codex', model: 'gpt-6.1-sol' }, hubText({ detail: `10 spawns in the last hour; the next slot opens at ${slotUtc} UTC`, expires_at: new Date(now + 6 * 60 * MINUTE).toISOString(), not_before: nextSlot.toISOString(), position: 1, queue_id: api.id(), reason: 'rate', status: 'queued' }, 'The spawn is queued; a [canterlot] message tells you when the session starts or fails. Do not retry.'));
  feed.text(run, 'Сверяюсь со списком сессий.');
  feed.call(run, 'list', 'mcp__canterlot__sessions_list', {}, hubText({ queued_spawns: { count: 1, nearest: new Date(now + 25 * MINUTE).toISOString() }, sessions: [
    { id, is_self: true, title: 'Кантерлот', activity: { state: 'working' } },
    { id: child, title: 'Карточки ленты', activity: { state: 'working' } },
    { id: routed, title: 'Фикс скролла', activity: { state: 'waiting_for_user' } },
    { id: second, title: 'Ревью вёрстки', activity: { state: 'idle' } }
  ] }));
  feed.call(run, 'inspect', 'mcp__canterlot__session_inspect', { session_id: routed }, hubText({ activity: { state: 'waiting_for_user', evidence: { question: 'Какой порог?' } }, session: { id: routed, title: 'Фикс скролла' }, messages: [] }));
  feed.text(run, 'Отвечаю делегату и ставлю будильник на двоих.');
  feed.call(run, 'send', 'mcp__canterlot__session_send', { session_id: routed, text: 'Порог 48 px, как в scrollController. Прижатая лента остаётся у низа, пока владелец не прокрутит вверх больше чем на порог; новая строка не сдвигает читаемую.' }, hubText({ deduplicated: false, input_id: api.id(), subscribed: true, target_paused: false, target_stopped: false }, 'Queued. You are subscribed.'));
  feed.call(run, 'alarm', 'mcp__canterlot__alarm_set', { when: { kind: 'session_state', session_ids: [child, routed], states: ['idle'], all: true }, batch: true, deadline_seconds: 4 * 3600 }, hubText({ alarm_id: api.id(), condition: 'sessions settle in idle, all, batched', deadline_at: new Date(now + 4 * 60 * MINUTE).toISOString(), replaced: null, state: 'armed' }, 'Alarm armed. End your turn.'));
  feed.text(run, 'И сторож на файл сборки.');
  feed.call(run, 'alarmFile', 'mcp__canterlot__alarm_set', { when: { kind: 'file', path: '/tmp/gate.rc' }, key: 'gate' }, hubText({ alarm_id: api.id(), condition: 'file /tmp/gate.rc appears', deadline_at: new Date(now + 24 * 60 * MINUTE).toISOString(), fired_at: null, replaced: null, state: 'armed' }));
  feed.call(run, 'alarmList', 'mcp__canterlot__alarm_list', {}, hubText({ alarms: [
    { alarm_id: 'al-1', conversation_id: id, kind: 'condition', state: 'armed', condition: '', session_ids: [child, routed], states: ['idle'], only_on_timeout: false, batch: true, note_bytes: 0, created_at: new Date(now).toISOString(), deadline_at: new Date(now + 4 * 60 * MINUTE).toISOString() },
    { alarm_id: 'al-2', conversation_id: id, kind: 'condition', state: 'armed', key: 'gate', condition: '', path: '/tmp/gate.rc', session_ids: [], states: [], only_on_timeout: false, batch: false, note_bytes: 0, created_at: new Date(now).toISOString(), deadline_at: new Date(now + 24 * 60 * MINUTE).toISOString() }
  ] }));

  const second_run = api.id();
  feed.input('Посмотри, как это делают другие, и обнови историю в Taiga');
  feed.call(second_run, 'skill', 'Skill', { skill: 'superpowers:brainstorming' }, 'Launching skill: superpowers:brainstorming');
  feed.call(second_run, 'search', 'WebSearch', { query: 'tool call cards chat feed design' }, 'Web search results for query: "tool call cards chat feed design"\n\nLinks: [{"title":"Designing agent UIs","url":"https://example.com/agent-ui"},{"title":"Chat feeds","url":"https://example.org/feeds"},{"title":"Tool cards","url":"https://example.net/cards"}]\n\nSummary.');
  feed.text(second_run, 'Читаю документацию Svelte о границах ошибок.');
  feed.call(second_run, 'fetch', 'WebFetch', { url: 'https://svelte.dev/docs/svelte/svelte-boundary', prompt: 'Как ловить ошибки рендера?' }, '# <svelte:boundary>\n\nBoundaries let you guard against errors in part of your app.');
  feed.call(second_run, 'docs', 'mcp__plugin_context7_context7__query-docs', { libraryId: '/sveltejs/svelte', query: 'svelte:boundary failed snippet' }, 'Documentation…');
  feed.text(second_run, 'Обновляю историю и оставляю комментарий.');
  feed.call(second_run, 'taiga', 'mcp__plugin_taiga_taiga__taiga_userstory_update', { ref: 123, subject: 'Карточки инструментов в ленте', status: 'in progress' }, hubText({ id: 9123, ref: 123, subject: 'Карточки инструментов в ленте', permalink: 'https://taiga.example/project/canterlot/us/123' }));
  feed.call(second_run, 'taigaComment', 'mcp__plugin_taiga_taiga__taiga_comment_add', { object_type: 'task', ref: 45, text: 'Реестр готов' }, hubText({ id: 1 }));

  const third = api.id();
  feed.input('Покажи, что получилось');
  feed.call(third, 'todo2', 'TodoWrite', TODO_SECOND, 'Todos have been modified successfully.');
  feed.text(third, 'Снимок карточек для проверки:');
  feed.call(third, 'share', 'mcp__canterlot__share_file', { path: '/tmp/feed-cards/phone-dark.png', title: 'Карточки в ленте', note: 'Телефон, тёмная тема: запуск, задачи, лимиты.' }, hubText({ attachment_id: IMAGE_ID, kind: 'image', mime: 'image/png', name: 'phone-dark.png', note: 'Телефон, тёмная тема: запуск, задачи, лимиты.', size: 112_259, status: 'shared', title: 'Карточки в ленте' }, 'The owner sees this file in this turn.'), {
    files: [{ attachment_id: IMAGE_ID, name: 'phone-dark.png', mime: 'image/png', size: 112_259, title: 'Карточки в ленте', note: 'Телефон, тёмная тема: запуск, задачи, лимиты.', width: 390, height: 844 }]
  });
  feed.input('[canterlot] Session "Миграция лимитов" was accepted: it is stopped, archived, and its worktree is removed.', {
    origin: { kind: 'event', from: accepted, title: 'Миграция лимитов', chain_depth: 1, event: { kind: 'session_result', state: 'idle', session_id: accepted, commits: [{ sha: '1a2b3c4', title: 'Перенести лимиты' }, { sha: '5d6e7f8', title: 'Тесты' }, { sha: '9a0b1c2', title: 'Документация' }] } }
  } as Partial<HistoryInput>);
  const fourth = api.id();
  feed.text(fourth, 'Принимаю миграцию и чищу хвосты.');
  feed.call(fourth, 'accept', 'mcp__canterlot__session_accept', { session_id: accepted }, hubText({ status: 'stopping' }, 'The session is being stopped; its worktree is removed afterwards.'));
  feed.input('[canterlot] Session "Миграция лимитов" was accepted.', {
    origin: { kind: 'event', from: accepted, title: 'Миграция лимитов', chain_depth: 1, event: { kind: 'accepted', session_id: accepted, worktree: 'removed', branch: 'canterlot/5e1f0a2b', branch_kept: true } }
  } as Partial<HistoryInput>);
  const fifth = api.id();
  feed.call(fifth, 'nudge', 'mcp__canterlot__nudge_set', { session_id: child, max_nudges: 2, idle_minutes: 5 }, hubText({ rule: { idle_minutes: 5, max_consecutive: 2 }, state: 'watching' }));
  feed.call(fifth, 'stop', 'mcp__canterlot__session_stop', { session_id: second }, hubText({ session_id: second, status: 'stopping', worktree_kept: true }));
  feed.text(fifth, 'Обычные инструменты остаются прежними:');
  feed.call(fifth, 'bash', 'Bash', 'npm run lint:tokens', 'Token lint: PASS', { summary: 'Bash: npm run lint:tokens' });
  feed.call(fifth, 'unknown', 'mcp__github__create_issue', { title: 'Карточки ленты', body: '…' }, hubText({ number: 42 }), { summary: 'mcp__github__create_issue: Карточки ленты' });
  feed.text(fifth, 'Готово: карточки на месте.');

  api.histories.set(id, feed.items);
  api.bindConversation(id);
  // Журнал хаба идёт дальше засеянной истории: новые события ленты не выглядят уже прочитанными.
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, child, routed, second, accepted, run, blocks: feed.blocks, imageId: IMAGE_ID };
}

/** Живое состояние делегата меняется событием хаба: строка списка сессий и лента. */
export function setActivity(hub: FakeHub, api: FakeHubApi, id: string, state: string, evidence: Record<string, unknown> = {}): void {
  const activity = { state, evidence, since: new Date().toISOString() };
  const row = api.list.get(id);
  if (row) row.activity = activity as typeof row.activity;
  const payload = { conversation_id: id, activity, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: new Date().toISOString(), settings: { model: row?.model ?? 'opus', effort: 'high', permission_mode: 'owner' }, crunch: { enabled: false } };
  hub.append('conversations', { kind: 'derived_changed', conversation_id: id, payload });
  hub.append(`conversation:${id}`, { kind: 'derived_changed', conversation_id: id, payload });
  hub.flush();
}

const sha = (text: string): string => createHash('sha256').update(text).digest('hex');

/** Новый вызов на глазах владельца: начало, затем результат. */
export function streamCall(hub: FakeHub, api: FakeHubApi, id: string, run: string, tool: string, input: unknown, opts: { blockId?: string; result?: string; resultId?: string } = {}): { call: string; result: string } {
  const call = opts.blockId ?? api.id();
  const result = opts.resultId ?? api.id();
  const scope = `conversation:${id}`;
  const content = JSON.stringify(input);
  // Без входа приходит только результат уже показанного вызова.
  if (input !== null) {
    api.blocks.set(call, { text: content, rev: 1, state: 'final' });
    hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { block_id: call, rev: 0, kind: 'tool_call', input_command_id: run, parent_block_id: null, agent: 'orchestrator', model: MODEL, tool_name: tool, summary: tool, approved_by: null } });
    hub.append(scope, { kind: 'block_final', conversation_id: id, payload: { block_id: call, rev: 1, text: content, checksum: sha(content), text_omitted: false, summary: tool } });
  }
  if (opts.result !== undefined) {
    api.blocks.set(result, { text: opts.result, rev: 1, state: 'final' });
    hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { block_id: result, rev: 0, kind: 'tool_result', input_command_id: run, parent_block_id: call, agent: 'orchestrator', model: MODEL, tool_name: null, summary: null, approved_by: null } });
    hub.append(scope, { kind: 'block_final', conversation_id: id, payload: { block_id: result, rev: 1, text: opts.result, checksum: sha(opts.result), text_omitted: false, summary: null } });
  }
  hub.flush();
  return { call, result };
}
