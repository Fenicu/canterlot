import { describe, expect, it } from 'vitest';
import { BlockModel } from '../../../../src/lib/stores/blocks';
import type { TimelineEntry } from '../../../../src/lib/stores/timeline';
import { cardCall } from '../../../../src/lib/timeline/cards/call';
import { planBefore } from '../../../../src/lib/timeline/cards/context';
import { planError, updateTail, type PlanAddData, type PlanAdoptData, type PlanListData, type PlanUpdateData } from '../../../../src/lib/timeline/cards/defs/plan';
import type { SpawnData } from '../../../../src/lib/timeline/cards/defs/sessions';
import { resolveCard } from '../../../../src/lib/timeline/cards/registry';
import type { SessionFacts } from '../../../../src/lib/timeline/cards/types';
import { callBlock, hub, made, resultBlock } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');
const ME = '01a1b3b9-0000-7000-8000-00000000aa01';
const OLD = '01a1b3b9-0000-7000-8000-00000000aa02';
const rows: Record<string, SessionFacts> = {
  [OLD]: { id: OLD, title: 'Старый оркестратор', state: 'stopped', accepted: false, harness: 'claude_code', model: 'opus' }
};
const session = (id: string) => rows[id] ?? null;

function card<D>(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: D }) | null;
}

function task(number: number, title: string, state: string, extra: Record<string, unknown> = {}) {
  return { id: `t-${number}`, orchestrator_id: ME, number, title, note: null, blocker: null, findings: [], state, executor_id: null, attempts: 0, created_at: '', updated_at: '', revision: 1, ...extra };
}

function failed(tool: string, input: unknown, output: string, root = true) {
  return cardCall(callBlock(tool, input), resultBlock(output, { summary: 'error' }), { root, conversationId: ME, session });
}

describe('plan_add', () => {
  const input = { tasks: [{ title: 'Миграция' }, { title: 'Хранилище' }, { title: 'Документация' }] };

  it('names_the_count_with_plurals_and_links_the_plan', () => {
    const answer = hub({ numbers: [6, 7, 8], tasks: [task(6, 'Миграция', 'waiting'), task(7, 'Хранилище', 'waiting'), task(8, 'Документация', 'waiting')] }, 'Pass plan_task to session_spawn…');
    const resolved = card<PlanAddData>(made('mcp__canterlot__plan_add', input, answer, { conversationId: ME }))!;
    expect(resolved.line).toBe('План: добавлены 3 задачи');
    expect(resolved.link).toEqual({ text: 'План', href: `/plan/${ME}`, plan: ME });
    expect(resolved.data.tasks.map((item) => `#${item.number} ${item.title}`)).toEqual(['#6 Миграция', '#7 Хранилище', '#8 Документация']);
    expect(resolved.word).toBeNull();
  });

  it('agrees_one_and_many', () => {
    const one = card<PlanAddData>(made('canterlot_plan_add', { tasks: [{ title: 'A' }] }, hub({ numbers: [1], tasks: [task(1, 'A', 'waiting')] })))!;
    expect(one.line).toBe('План: добавлена 1 задача');
    const titles = Array.from({ length: 5 }, (_, index) => ({ title: `T${index}` }));
    const five = card<PlanAddData>(made('canterlot.plan_add', { tasks: titles }, hub({ numbers: [1, 2, 3, 4, 5], tasks: titles.map((item, index) => task(index + 1, item.title, 'waiting')) })))!;
    expect(five.line).toBe('План: добавлено 5 задач');
  });

  it('a_running_call_counts_the_input', () => {
    const resolved = card<PlanAddData>(made('mcp__canterlot__plan_add', input, null, { working: true }))!;
    expect(resolved.line).toBe('План: добавление 3 задач');
    expect(resolved.word).toMatchObject({ word: 'идёт' });
  });

  it('plan_full_reads_in_russian', () => {
    const resolved = card<PlanAddData>(failed('mcp__canterlot__plan_add', input, 'plan_full: This plan already has 199 tasks; a plan holds at most 200. Cancel or adopt into a fresh session instead.'))!;
    expect(resolved.line).toBe('План: задачи не добавлены');
    expect(resolved.word).toMatchObject({ word: 'ошибка', tone: 'danger' });
    expect(resolved.data.error).toEqual({ code: 'plan_full', text: 'План полон: в нём 199 задач, вмещается 200' });
  });
});

describe('plan_update', () => {
  it('shows_the_transition_from_the_previous_plan_call', () => {
    const answer = hub({ status: 'updated', task: task(3, 'Перевод в in_review', 'blocked', { blocker: 'Нужен общий ответ гейта' }) });
    const call = made('mcp__canterlot__plan_update', { number: 3, blocker: 'Нужен общий ответ гейта' }, answer, { conversationId: ME, planBefore: (n) => (n === 3 ? { title: 'Перевод в in_review', state: 'in_progress' } : null) });
    const resolved = card<PlanUpdateData>(call)!;
    expect(resolved.line).toBe('#3 Перевод в in_review: в работе → заблокирована');
    expect(resolved.data.blocker).toBe('Нужен общий ответ гейта');
  });

  it('without_history_names_only_the_new_state', () => {
    const answer = hub({ status: 'updated', task: task(2, 'Хранилище', 'in_review', { findings: ['Снимок заменяет набор', 'Ревизия защищает'] }) });
    const resolved = card<PlanUpdateData>(made('mcp__canterlot__plan_update', { number: 2, state: 'in_review', findings: ['Снимок заменяет набор', 'Ревизия защищает'] }, answer))!;
    expect(resolved.line).toBe('#2 Хранилище: на проверке');
    expect(resolved.data.findings).toHaveLength(2);
    expect(resolved.data.edits).toEqual(['findings']);
  });

  it('edits_without_a_state_say_what_changed', () => {
    const answer = hub({ status: 'updated', task: task(4, 'Документация API', 'waiting', { note: 'Сначала схема' }) });
    const resolved = card<PlanUpdateData>(made('mcp__canterlot__plan_update', { number: 4, note: 'Сначала схема', blocker: '' }, answer))!;
    expect(resolved.line).toBe('#4 Документация API: заметка, блокер снят');
    const same = card<PlanUpdateData>(made('mcp__canterlot__plan_update', { number: 4, state: 'waiting' }, hub({ status: 'unchanged', task: task(4, 'Документация API', 'waiting') })))!;
    expect(same.line).toBe('#4 Документация API: без изменений');
    expect(updateTail({ ...same.data, unchanged: false, to: null, edits: [] })).toBe('обновлена');
  });

  it('errors_read_in_russian', () => {
    const missing = card<PlanUpdateData>(failed('mcp__canterlot__plan_update', { number: 9, state: 'done' }, 'plan_task_not_found: Your plan has no task #9; plan_list shows the numbers'))!;
    expect(missing.line).toBe('План: задача #9 не обновлена');
    expect(missing.data.error?.text).toBe('В плане нет задачи #9');
    const bad = card<PlanUpdateData>(failed('mcp__canterlot__plan_update', { number: 2, title: '' }, 'invalid_arguments: title must not be empty'))!;
    expect(bad.data.error?.text).toBe('Неверный вызов: пустое название задачи');
  });
});

describe('plan_list', () => {
  it('summarises_states_in_screen_order', () => {
    const tasks = [task(1, 'A', 'in_progress'), task(2, 'B', 'in_review'), task(3, 'C', 'done'), task(4, 'D', 'in_progress'), task(5, 'E', 'done'), task(6, 'F', 'done'), task(7, 'G', 'done')];
    const resolved = card<PlanListData>(made('mcp__canterlot__plan_list', {}, hub({ tasks, hint: '#1 [in_progress] A' }), { conversationId: ME }))!;
    expect(resolved.line).toBe('План: 2 в работе · 1 на проверке · 4 готово');
    expect(resolved.data.tasks).toHaveLength(7);
    expect(resolved.link?.href).toBe(`/plan/${ME}`);
  });

  it('empty_plan_and_plurals', () => {
    expect(card<PlanListData>(made('mcp__canterlot__plan_list', {}, hub({ tasks: [], hint: 'Your plan is empty.' })))!.line).toBe('План пуст');
    const waiting = [task(1, 'A', 'waiting'), task(2, 'B', 'waiting'), task(3, 'C', 'blocked')];
    expect(card<PlanListData>(made('mcp__canterlot__plan_list', {}, hub({ tasks: waiting })))!.line).toBe('План: 1 заблокирована · 2 ждут');
  });

  it('garbage_answer_falls_back', () => {
    expect(resolveCard(made('mcp__canterlot__plan_list', {}, 'not json'), NOW)).toBeNull();
  });
});

describe('plan_adopt', () => {
  it('names_the_source_and_the_count_in_her_voice', () => {
    const answer = hub({ moved: [{ from: 2, to: 8 }, { from: 3, to: 9 }], hint: 'Moved 2 tasks.' });
    const resolved = card<PlanAdoptData>(made('mcp__canterlot__plan_adopt', { from_session_id: OLD }, answer, { session, conversationId: ME }))!;
    expect(resolved.line).toBe('Забрала план «Старый оркестратор»: 2 задачи');
    expect(resolved.link).toEqual({ text: '«Старый оркестратор»', href: `/c/${OLD}` });
    expect(resolved.data.moved).toEqual([{ from: 2, to: 8 }, { from: 3, to: 9 }]);
  });

  it('nothing_moved_and_neutral_voice', () => {
    const none = card<PlanAdoptData>(made('mcp__canterlot__plan_adopt', { from_session_id: OLD }, hub({ moved: [], hint: 'The source has no open tasks' }), { session, root: false }))!;
    expect(none.line).toBe('План «Старый оркестратор» перенесён: открытых задач нет');
  });

  it('refusal_reads_in_russian', () => {
    const resolved = card<PlanAdoptData>(failed('mcp__canterlot__plan_adopt', { from_session_id: OLD }, 'invalid_arguments: The source is a delegated session; only a root session\'s plan can be adopted'))!;
    expect(resolved.line).toBe('Не забрала план «Старый оркестратор»');
    expect(resolved.data.error?.text).toBe('Неверный вызов: план делегата забрать нельзя');
  });
});

describe('session_spawn with plan_task', () => {
  it('carries_the_task_number_and_link_outcome', () => {
    const answer = hub({ session_id: 'child', status: 'started', plan_task: { number: 3, linked: true } }, 'Linked to plan task #3');
    const resolved = card<SpawnData>(made('mcp__canterlot__session_spawn', { title: 'Гейт', brief: 'x', plan_task: 3 }, answer))!;
    expect(resolved.data.planTask).toEqual({ number: 3, linked: true });
    const loose = card<SpawnData>(made('mcp__canterlot__session_spawn', { title: 'Гейт', brief: 'x', plan_task: 3 }, hub({ session_id: 'child', status: 'started', plan_task: { number: 3, linked: false } })))!;
    expect(loose.data.planTask?.linked).toBe(false);
    expect(card<SpawnData>(made('mcp__canterlot__session_spawn', { title: 'Гейт', brief: 'x' }, hub({ session_id: 'c', status: 'started' })))!.data.planTask).toBeNull();
  });

  it('a_closed_task_refusal_reads_in_russian', () => {
    const resolved = card<SpawnData>(failed('mcp__canterlot__session_spawn', { title: 'Гейт', brief: 'x', plan_task: 5 }, 'plan_task_closed: Task #5 is done; reopen it with plan_update first'))!;
    expect(resolved.line).toBe('Не запустила «Гейт»');
    expect(resolved.data.error).toBe('Задача #5 уже готова: сначала её открывают заново');
  });
});

describe('plan errors', () => {
  it('maps_every_code', () => {
    expect(planError('plan_task_closed: Task #2 is cancelled; reopen it with plan_update first').text).toBe('Задача #2 уже отменена: сначала её открывают заново');
    expect(planError('invalid_arguments: pass 1 to 20 tasks').text).toBe('Неверный вызов: за раз добавляют от 1 до 20 задач');
    expect(planError('invalid_arguments: something new').text).toBe('Неверный вызов: аргументы не подошли хабу');
    expect(planError('session_not_found: no such session').text).toBe('Такой сессии нет у владельца');
    expect(planError('weird_code: Something').text).toBe('Something');
  });
});

describe('plan state before a call', () => {
  function entry(block: BlockModel): TimelineEntry {
    return { kind: 'block', block } as unknown as TimelineEntry;
  }
  function pair(tool: string, input: unknown, output: string): BlockModel[] {
    const call = callBlock(tool, input);
    return [call, resultBlock(output, { parentId: call.id } as Partial<BlockModel>)];
  }

  it('reads_the_latest_earlier_answer_and_stops_at_an_adopt', () => {
    const added = pair('mcp__canterlot__plan_add', { tasks: [{ title: 'A' }] }, hub({ numbers: [3], tasks: [task(3, 'Гейт', 'waiting')] }));
    const started = pair('mcp__canterlot__plan_update', { number: 3, state: 'in_progress' }, hub({ status: 'updated', task: task(3, 'Гейт', 'in_progress') }));
    const target = callBlock('mcp__canterlot__plan_update', { number: 3, state: 'blocked' });
    const entries = [...added, ...started, target].map(entry);
    expect(planBefore(entries, target, 3)).toEqual({ title: 'Гейт', state: 'in_progress' });
    expect(planBefore(entries, started[0]!, 3)).toEqual({ title: 'Гейт', state: 'waiting' });
    const adopted = pair('mcp__canterlot__plan_adopt', { from_session_id: OLD }, hub({ moved: [{ from: 1, to: 3 }] }));
    expect(planBefore([...added, ...adopted, target].map(entry), target, 3)).toBeNull();
  });
});
