import { describe, expect, it } from 'vitest';
import { resolveCard, cardIds, cardStays, cardQuiet } from '../../../../src/lib/timeline/cards/registry';
import { spawnWord, queueLine, placeLine, type SpawnData, acceptFacts, acceptWord, type AcceptData } from '../../../../src/lib/timeline/cards/defs/sessions';
import type { TodoData } from '../../../../src/lib/timeline/cards/defs/todo';
import type { LimitsData } from '../../../../src/lib/timeline/cards/defs/limits';
import type { SessionFacts } from '../../../../src/lib/timeline/cards/types';
import type { HubEvent } from '../../../../src/lib/timeline/hubEvent';
import { callBlock, hub, made, resultBlock } from './helpers';
import { cardCall } from '../../../../src/lib/timeline/cards/call';

const NOW = Date.parse('2026-11-07T10:05:00Z');
const CHILD = '01a1b3b9-406e-7682-a3db-31c68b993844';
const rows: Record<string, SessionFacts> = {
  [CHILD]: { id: CHILD, title: 'Карточки ленты', state: 'working', accepted: false, harness: 'claude_code', model: 'opus[1m]' },
  t5: { id: 't5', title: 'T5', state: 'idle', accepted: false, harness: 'codex', model: 'gpt-6.1-sol' },
  t6: { id: 't6', title: 'T6', state: 'working', accepted: false, harness: 'codex', model: 'gpt-6.1-sol' }
};
const session = (id: string) => rows[id] ?? null;

function card<D>(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: D }) | null;
}

const BRIEF = 'Сделай карточки ленты\n\nПодробности: реестр, нормализатор, тесты.';

describe('delegate start', () => {
  const input = { title: 'Карточки ленты', brief: BRIEF, harness: 'claude_code', model: 'opus[1m]', effort: 'high', isolated: true };

  it('started_reads_in_her_voice_with_executor_branch_and_live_child_state', () => {
    const answer = hub({ permission_mode: 'bypass', session_id: CHILD, status: 'started', subscribed: true, worktree: { branch: 'canterlot/8b993844', path: '/w' } }, 'Session created.');
    const resolved = card<SpawnData>(made('mcp__canterlot__session_spawn', input, answer, { session }))!;
    expect(resolved.line).toBe('Запустила «Карточки ленты»');
    expect(resolved.data.executor).toBe('Opus 1M · высокое');
    expect(placeLine(resolved.data)).toBe('отдельная копия, ветка canterlot/8b993844');
    expect(resolved.link).toEqual({ text: '«Карточки ленты»', href: `/c/${CHILD}` });
    expect(spawnWord(made('canterlot_session_spawn', input, answer, { session }), resolved.data, NOW)).toEqual({ word: 'работает', tone: 'accent', live: true });
  });

  it('a_delegate_speaks_neutrally', () => {
    const answer = hub({ session_id: CHILD, status: 'started' });
    const call = made('canterlot.session_spawn', input, answer, { session, root: false });
    expect(resolveCard(call, NOW)!.line).toBe('Запуск делегата «Карточки ленты»');
  });

  it('routed_start_takes_the_executor_from_the_hub_choice', () => {
    const answer = hub({ routing: { effort: 'high', harness: 'opencode', model: 'opencode-go/deepseek-v4.1-flash', policy_revision: 0, task_type: 'bugfix' }, session_id: CHILD, status: 'started' });
    const resolved = card<SpawnData>(made('mcp__canterlot__session_spawn', { brief: 'x', route: { task_type: 'bugfix' } }, answer))!;
    expect(resolved.data.route).toBe('bugfix');
    expect(resolved.data.executor).toBe('deepseek-v4.1-flash · высокое');
    expect(resolved.data.routing).not.toBeNull();
  });

  it('pending_approval_waits_for_the_owner_and_turns_live_after_the_hub_event', () => {
    const answer = hub({ request_id: 'req-1', status: 'pending_approval' }, "This harness or model is outside the owner's list.");
    const call = made('mcp__canterlot__session_spawn', input, answer, { session });
    const resolved = card<SpawnData>(call)!;
    expect(resolved.line).toBe('Попросила разрешения на запуск «Карточки ленты»');
    expect(spawnWord(call, resolved.data, NOW)).toMatchObject({ word: 'ждёт владельца', tone: 'warning' });
    const allowed: HubEvent = { kind: 'approval', decision: 'allowed', request_id: 'req-1', session_id: CHILD } as HubEvent;
    const later = made('mcp__canterlot__session_spawn', input, answer, { session, events: () => [allowed] });
    const after = card<SpawnData>(later)!;
    expect(after.data.status).toBe('started');
    expect(spawnWord(later, after.data, NOW).word).toBe('работает');
  });

  it('queued_shows_the_reason_and_the_next_slot', () => {
    const answer = hub({ detail: '10 spawns in the last hour; the next slot opens at 14:32 UTC', expires_at: '2026-11-08T10:00:00Z', not_before: '2026-11-07T10:32:00Z', position: 1, queue_id: 'q', reason: 'rate' });
    const call = made('mcp__canterlot__session_spawn', input, answer.replace('"position"', '"status": "queued",\n  "position"'));
    const resolved = card<SpawnData>(call)!;
    expect(resolved.line).toBe('Поставила в очередь «Карточки ленты»');
    expect(queueLine(resolved.data, NOW)).toMatch(/^Запуск в очереди: частота запусков/);
    expect(spawnWord(call, resolved.data, NOW).word).toMatch(/^следующий в \d{2}:\d{2}$/);
  });

  it('error_names_the_hub_message', () => {
    const call = cardCall(callBlock('mcp__canterlot__session_spawn', input), resultBlock('spawn_depth_exceeded: Spawn depth limit reached', { summary: 'error' }), { root: true });
    const resolved = card<SpawnData>(call)!;
    expect(resolved.line).toBe('Не запустила «Карточки ленты»');
    expect(resolved.data.error).toBe('Spawn depth limit reached');
  });

  it('a_brief_cut_by_the_history_page_still_shows_its_first_line', () => {
    const text = JSON.stringify({ ...input, brief: `${BRIEF}\n${'x'.repeat(5000)}` }).slice(0, 4096);
    const call = cardCall(callBlock('mcp__canterlot__session_spawn', text, { truncated: true }), resultBlock(hub({ session_id: CHILD, status: 'started' })), { root: true });
    const resolved = card<SpawnData>(call)!;
    expect(resolved.data.briefCut).toBe(true);
    expect(resolved.data.brief.startsWith('Сделай карточки ленты')).toBe(true);
  });

  it('running_start_has_no_answer_yet', () => {
    const block = callBlock('mcp__canterlot__session_spawn', input);
    const call = cardCall(block, null, { root: true, working: true });
    const resolved = card<SpawnData>(call)!;
    expect(resolved.data.status).toBe('running');
    expect(resolved.line).toBe('Запускает «Карточки ленты»');
  });
});

describe('accept', () => {
  it('reads_the_outcome_from_the_hub_events_of_the_feed', () => {
    const events: HubEvent[] = [
      { kind: 'session_result', session_id: CHILD, commits: [{ sha: 'a', subject: 'x' }, { sha: 'b', subject: 'y' }, { sha: 'c', subject: 'z' }] } as unknown as HubEvent,
      { kind: 'accepted', session_id: CHILD, worktree: 'removed', branch: 'canterlot/8b993844', branch_kept: true } as HubEvent
    ];
    const call = made('mcp__canterlot__session_accept', { session_id: CHILD }, hub({ status: 'stopping' }, 'The session is being stopped.'), { session, events: () => events });
    const resolved = card<AcceptData>(call)!;
    expect(resolved.line).toBe('Приняла «Карточки ленты»');
    expect(acceptFacts(resolved.data)).toEqual(['3 коммита', 'рабочая копия убрана', 'ветка canterlot/8b993844 оставлена']);
    expect(acceptWord(call, resolved.data).word).toBe('остановлена');
  });

  it('stopping_turns_into_stopped_when_the_child_row_stops', () => {
    const live = { ...rows[CHILD]!, state: 'working' };
    const call = made('canterlot_session_accept', { session_id: CHILD }, hub({ status: 'stopping' }), { session: () => live, root: false });
    const resolved = card<AcceptData>(call)!;
    expect(resolved.line).toBe('Приёмка «Карточки ленты»');
    expect(acceptWord(call, resolved.data).word).toBe('останавливается');
    live.state = 'stopped';
    expect(acceptWord(call, resolved.data).word).toBe('остановлена');
  });
});

describe('send and look-ups', () => {
  it('send_names_the_target_and_subscription', () => {
    const call = made('mcp__canterlot__session_send', { session_id: CHILD, text: 'Продолжай со следующего пункта' }, hub({ deduplicated: false, input_id: 'i', subscribed: true, target_paused: false, target_stopped: false }), { session });
    const resolved = resolveCard(call, NOW)!;
    expect(resolved.line).toBe('Написала «Карточки ленты»');
    expect((resolved.data as { subscribed: boolean }).subscribed).toBe(true);
  });

  it('inspect_reads_the_state_word', () => {
    const answer = hub({ activity: { state: 'waiting_for_user', evidence: {} }, session: { id: CHILD, title: 'Карточки ленты' }, messages: [] });
    expect(resolveCard(made('mcp__canterlot__session_inspect', { session_id: CHILD }, answer), NOW)!.line).toBe('Посмотрела «Карточки ленты» · задала вопрос');
  });

  it('sessions_list_counts_states_without_itself', () => {
    const sessions = [
      { id: 'me', is_self: true, title: 'Я', activity: { state: 'working' } },
      { id: 'a', title: 'A', activity: { state: 'working' } },
      { id: 'b', title: 'B', activity: { state: 'working' } },
      { id: 'c', title: 'C', activity: { state: 'working' } },
      { id: 'd', title: 'D', activity: { state: 'idle' } },
      { id: 'e', title: 'E', activity: { state: 'idle' } }
    ];
    const line = resolveCard(made('canterlot_sessions_list', {}, hub({ queued_spawns: { count: 0, nearest: null }, sessions })), NOW)!.line;
    expect(line).toBe('Сессии: 3 работают, 2 ждут сообщения');
  });

  it('wait_reports_what_it_waited_for', () => {
    const line = resolveCard(made('mcp__canterlot__session_wait', { session_id: CHILD }, hub({ event_suppressed: false, state: 'idle', summary: '…', timed_out: false }), { session }), NOW)!.line;
    expect(line).toBe('Дождалась «Карточки ленты» · завершена');
  });

  it('control_tools_say_one_fact', () => {
    expect(resolveCard(made('mcp__canterlot__session_stop', { session_id: CHILD }, hub({ session_id: CHILD, status: 'stopping', worktree_kept: true }), { session }), NOW)!.line).toBe('Остановила «Карточки ленты»');
    expect(resolveCard(made('mcp__canterlot__session_interrupt', { session_id: CHILD }, hub({ status: 'interrupted' }), { session, root: false }), NOW)!.line).toBe('Прерывание хода «Карточки ленты»');
    expect(resolveCard(made('mcp__canterlot__nudge_set', { session_id: CHILD, max_nudges: 2, idle_minutes: 5 }, hub({ rule: {}, state: 'watching' }), { session }), NOW)!.line).toBe('Подталкивание «Карточки ленты» · до 2 раз подряд, после 5 мин тишины');
    expect(resolveCard(made('mcp__canterlot__spawn_cancel', { queue_id: 'q' }, hub({ queue_id: 'q', status: 'cancelled' })), NOW)!.line).toBe('Отменила запуск из очереди');
    expect(resolveCard(made('mcp__canterlot__route_preview', { task_type: 'bugfix' }, hub({ decision: { harness: 'codex', model: 'gpt-6.1-sol', effort: 'high' } })), NOW)!.line).toBe('Маршрут «bugfix»: gpt-6.1-sol · высокое');
    expect(resolveCard(made('mcp__canterlot__canterlot_preset_apply', { id: 'clean' }, hub({ applies: 'next_turn', id: 'clean', revision: null })), NOW)!.line).toBe('Применила пресет «Без инструкций»');
    expect(resolveCard(made('mcp__canterlot__session_subscribe', { session_id: CHILD }, hub({ current_state: 'working', subscribed: true }), { session }), NOW)!.line).toBe('Подписалась на «Карточки ленты»');
  });
});

describe('todo', () => {
  const before = { todos: [{ content: 'Нормализатор', status: 'in_progress', activeForm: 'Пишу' }, { content: 'Реестр', status: 'pending', activeForm: 'Пишу' }, { content: 'Тесты', status: 'pending', activeForm: 'Пишу' }] };
  const after = { todos: [{ content: 'Нормализатор', status: 'completed', activeForm: 'Пишу' }, { content: 'Реестр', status: 'in_progress', activeForm: 'Пишу' }, { content: 'Тесты', status: 'pending', activeForm: 'Пишу' }, { content: 'Видео', status: 'pending', activeForm: 'Пишу' }] };

  it('diffs_against_the_previous_call_of_the_same_lane', () => {
    const previous = callBlock('TodoWrite', before);
    const call = made('TodoWrite', after, 'Todos have been modified successfully.', { previous: () => previous });
    const resolved = card<TodoData>(call)!;
    expect(resolved.data.done).toBe(1);
    expect(resolved.data.total).toBe(4);
    expect(resolved.data.closed.map((item) => item.text)).toEqual(['Нормализатор']);
    expect(resolved.data.added.map((item) => item.text)).toEqual(['Видео']);
    expect(resolved.data.current?.text).toBe('Реестр');
    expect(resolved.line).toBe('Задачи · 1 из 4 · готово «Нормализатор» · новых 1');
  });

  it('opencode_list_without_a_previous_call_has_no_diff_and_counts_cancelled_out', () => {
    const call = made('todowrite', { todos: [{ content: 'a', status: 'completed', priority: 'high', id: '1' }, { content: 'b', status: 'cancelled', priority: 'low', id: '2' }, { content: 'c', status: 'pending', priority: 'low', id: '3' }] }, '[]');
    const resolved = card<TodoData>(call)!;
    expect(resolved.data.compared).toBe(false);
    expect(resolved.line).toBe('Задачи · 1 из 2');
  });
});

describe('alarms', () => {
  it('session_alarm_uses_titles_and_plural_verbs', () => {
    const answer = hub({ alarm_id: 'a1', condition: 'sessions t5, t6 settle in idle; deadline', deadline_at: '2026-11-07T18:00:00.000Z', replaced: null, state: 'armed' }, 'Alarm armed.');
    const call = made('mcp__canterlot__alarm_set', { when: { kind: 'session_state', session_ids: ['t5', 't6'], states: ['idle'], all: true } }, answer, { session });
    const line = resolveCard(call, NOW)!.line;
    expect(line).toMatch(/^Будильник · когда «T5» и «T6» закончат · до \d{2}:\d{2}$/);
  });

  it('file_alarm_reuses_the_alarm_strip_wording', () => {
    const answer = hub({ alarm_id: 'a2', condition: 'file /tmp/gate.rc appears', deadline_at: '2026-11-07T18:00:00.000Z', fired_at: null, replaced: null, state: 'armed' });
    const line = resolveCard(made('mcp__canterlot__alarm_set', { when: { kind: 'file', path: '/tmp/gate.rc' } }, answer), NOW)!.line;
    expect(line).toMatch(/^Будильник · когда появится \/tmp\/gate\.rc · до \d{2}:\d{2}$/);
  });

  it('cancel_and_list', () => {
    expect(resolveCard(made('mcp__canterlot__alarm_cancel', { key: 'gate' }, hub({ alarm: null, result: 'cancelled' })), NOW)!.line).toBe('Будильник «gate» отменён');
    const alarms = [{ alarm_id: 'x', kind: 'at', state: 'armed', fires_at: '2026-11-07T15:00:00.000Z', session_ids: [], states: [], only_on_timeout: false, batch: false, condition: 'at', note_bytes: 0, conversation_id: 'c', created_at: '2026-11-07T10:00:00Z' }];
    expect(resolveCard(made('mcp__canterlot__alarm_list', {}, hub({ alarms })), NOW)!.line).toMatch(/^Будильники: 1 взведён · ближайший в \d{2}:\d{2}$/);
  });
});

describe('share, limits, web, tools', () => {
  it('share_file_takes_title_and_files_of_the_block', () => {
    const block = callBlock('mcp__canterlot__share_file', { path: '/tmp/feed-cards/video.mp4', title: 'Видео карточек', note: 'Телефон, тёмная тема' });
    block.files = [{ attachment_id: 'att', name: 'video.mp4', mime: 'video/mp4', size: 4_000_000, title: 'Видео карточек', note: 'Телефон, тёмная тема' }];
    const call = cardCall(block, resultBlock(hub({ attachment_id: 'att', kind: 'video', mime: 'video/mp4', name: 'video.mp4', note: '', size: 4_000_000, status: 'shared', title: '' })), { root: true });
    const resolved = resolveCard(call, NOW)!;
    expect(resolved.line).toBe('Передала файл «Видео карточек»');
    expect((resolved.data as { note: string }).note).toBe('Телефон, тёмная тема');
  });

  it('limits_parse_the_report_text', () => {
    const text = 'Claude API budget: five-hour window 15% used, resets 2026-10-31 11:20 UTC; weekly window 39% used, resets 2026-11-03 01:00 UTC.\nCodex API budget: five-hour window 4.1% used; weekly window 83% used, resets 2026-11-10 08:00 UTC.\nOpenCode API budget: no limit data yet.\nObserved: claude just now, codex 29 min ago.';
    const resolved = card<LimitsData>(made('mcp__canterlot__canterlot_limits', {}, text))!;
    expect(resolved.data.rows.map((row) => row.label)).toEqual(['Claude', 'Codex', 'OpenCode']);
    expect(resolved.data.rows[1]!.meters[1]).toMatchObject({ window: 'week', text: '17%', tone: 'warning' });
    expect(resolved.line).toBe('Лимиты · Claude 5ч 85%, нед 61% · Codex 5ч 96%, нед 17% · OpenCode нет данных');
  });

  it('web_search_counts_links_and_fetch_names_the_domain', () => {
    const result = 'Web search results for query: "svelte boundary"\n\nLinks: [{"title":"Boundary","url":"https://svelte.dev/docs/svelte/svelte-boundary"},{"title":"x","url":"https://github.com/x"}]\n\nSummary';
    expect(resolveCard(made('WebSearch', { query: 'svelte boundary' }, result), NOW)!.line).toBe('Искала: svelte boundary · 2 ссылки');
    const page = resolveCard(made('WebFetch', { url: 'https://www.svelte.dev/docs/svelte/svelte-boundary', prompt: 'x' }, '# <svelte:boundary>\n\nText'), NOW)!;
    expect(page.line).toBe('Открыла svelte.dev · <svelte:boundary>');
    expect(page.link).toEqual({ text: 'svelte.dev', href: 'https://www.svelte.dev/docs/svelte/svelte-boundary', external: true });
  });

  it('skill_taiga_context7', () => {
    expect(resolveCard(made('Skill', { skill: 'superpowers:brainstorming' }, 'Launching skill'), NOW)!.line).toBe('Навык: brainstorming');
    expect(resolveCard(made('skill', { name: 'brainstorming' }, ''), NOW)!.line).toBe('Навык: brainstorming');
    const story = resolveCard(made('mcp__plugin_taiga_taiga__taiga_userstory_update', { ref: 123, subject: 'Карточки ленты' }, hub({ ref: 123, subject: 'Карточки ленты', permalink: 'https://taiga.example/project/x/us/123' })), NOW)!;
    expect(story.line).toBe('Taiga · обновила историю #123 «Карточки ленты»');
    expect(story.link).toEqual({ text: '#123', href: 'https://taiga.example/project/x/us/123', external: true });
    expect(resolveCard(made('mcp__plugin_taiga_taiga__taiga_comment_add', { object_type: 'task', ref: 45, text: 'ok' }, '{}'), NOW)!.line).toBe('Taiga · комментарий к задаче #45');
    expect(resolveCard(made('taiga_taiga_userstory_update', { ref: 7 }, '{}', { root: false }), NOW)!.line).toBe('Taiga · обновление истории #7');
    expect(resolveCard(made('mcp__plugin_context7_context7__query-docs', { libraryId: '/vercel/next.js', query: 'routing' }, 'docs'), NOW)!.line).toBe('Документация: next.js — routing');
    expect(resolveCard(made('mcp__plugin_context7_context7__resolve-library-id', { libraryName: 'svelte' }, 'ids'), NOW)!.line).toBe('Документация: поиск библиотеки svelte');
  });

  it('question_without_input_reads_the_answer_from_the_cli_text', () => {
    const resolved = resolveCard(made('AskUserQuestion', '', 'Your questions have been answered: "Продолжать проверку интерфейса?"="Продолжить". You can now continue.'), NOW)!;
    expect(resolved.line).toBe('Спросила: Продолжать проверку интерфейса?');
  });

  it('quiet_meta_calls_are_quiet', () => {
    const resolved = resolveCard(made('ToolSearch', { query: 'select:TodoWrite', max_results: 1 }, 'ok'), NOW)!;
    expect(resolved.line).toBe('Поиск инструментов: TodoWrite');
    expect(resolved.def.quiet).toBe(true);
    for (const name of ['ToolSearch', 'Monitor', 'TaskStop', 'ListAgents', 'SendMessage', 'PushNotification']) expect(cardQuiet({ kind: 'tool_call', toolName: name })).toBe(true);
    expect(cardStays({ kind: 'tool_call', toolName: 'mcp__canterlot__session_spawn' })).toBe(true);
  });
});

describe('fallback', () => {
  it('unknown_tools_and_generic_tools_have_no_card', () => {
    for (const name of ['Bash', 'Read', 'Edit', 'Write', 'mcp__github__create_issue', 'SomethingNew']) expect(resolveCard(made(name, { command: 'ls' }, 'out'), NOW)).toBeNull();
  });

  it('malformed_input_falls_back_to_the_generic_block', () => {
    // Вход не тот по форме: у списка задач нет списка, у сообщения нет ни сессии, ни текста.
    expect(resolveCard(made('TodoWrite', { todos: 'не список' }, 'ok'), NOW)).toBeNull();
    expect(resolveCard(made('mcp__canterlot__session_send', { nothing: true }, 'ok'), NOW)).toBeNull();
    expect(resolveCard(made('mcp__canterlot__session_spawn', 'not json at all', 'still not json'), NOW)).toBeNull();
    expect(resolveCard(made('mcp__canterlot__canterlot_limits', {}, 'garbage'), NOW)).toBeNull();
    expect(resolveCard(made('WebFetch', { url: 'not a url' }, ''), NOW)).toBeNull();
    expect(resolveCard(made('mcp__canterlot__session_inspect', [1, 2], ''), NOW)).toBeNull();
  });

  it('a_card_whose_logic_throws_is_a_fallback_not_a_crash', () => {
    const call = made('mcp__canterlot__session_wait', { session_id: CHILD }, hub({ state: 'idle' }), {
      session: () => {
        throw new Error('boom');
      }
    });
    expect(resolveCard(call, NOW)).toBeNull();
  });

  it('every_registered_card_survives_an_empty_call', () => {
    for (const id of cardIds()) {
      const name = id.startsWith('canterlot.') ? `mcp__canterlot__${id.slice(10) === 'limits' ? 'canterlot_limits' : id.slice(10)}` : id;
      expect(() => resolveCard(made(name, '', ''), NOW)).not.toThrow();
    }
  });
});
