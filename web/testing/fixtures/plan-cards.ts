// Лента оркестратора с вызовами доски плана: добавление, правка, чтение, перенос и запуск с задачей; удачи и отказы.
import type { PlanTask } from '../../src/lib/stores/plan.svelte';
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';
import { Feed, hubText } from './feed-cards';

const MINUTE = 60_000;
const MODEL = 'claude-opus-5-5';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };

export interface PlanCardsSeed {
  id: string;
  child: string;
  old: string;
  /** Задачи живого плана по номеру. */
  tasks: Record<number, PlanTask>;
  blocks: Record<string, string>;
}

const TITLES = ['Таблица plan_tasks и миграция', 'Хранилище плана и вкладка «План»', 'Перевод в in_review по подписке', 'Документация API', 'Карточки плана в ленте'];

/** Задача в ответе хаба: живая задача с состоянием на момент вызова. */
function asOf(task: PlanTask, patch: Partial<PlanTask>): PlanTask {
  return { ...task, ...patch };
}

export function seedPlanCards(api: FakeHubApi, opts: { now?: number } = {}): PlanCardsSeed {
  const now = opts.now ?? Date.now();
  const id = seedConversation(api, { title: 'Кантерлот', model: MODEL, harness: CLAUDE });
  const child = seedConversation(api, { title: 'Карточки плана', parent_id: id, spawn_depth: 1, model: 'opus[1m]', harness: CLAUDE, activity: { state: 'working', evidence: { phase: 'running_tool' }, since: new Date(now).toISOString() } as never });
  const old = seedConversation(api, { title: 'Прошлый оркестратор', model: MODEL, harness: CLAUDE, activity: { state: 'stopped', evidence: {}, since: new Date(now - 90 * MINUTE).toISOString() } as never });

  // Живой план сейчас: #1 уже готова, хотя в последнем чтении была в работе.
  const tasks: Record<number, PlanTask> = {};
  const push = (number: number, title: string, patch: Partial<PlanTask>) => {
    tasks[number] = api.pushPlanTask({ orchestrator_id: id, number, title, created_at: new Date(now - 60 * MINUTE).toISOString(), updated_at: new Date(now - 2 * MINUTE).toISOString(), ...patch });
  };
  push(1, TITLES[0]!, { state: 'done' });
  push(2, TITLES[1]!, { state: 'in_review', findings: ['Снимок заменяет набор задач целиком', 'Ревизия защищает от старых событий'] });
  push(3, TITLES[2]!, { state: 'blocked', blocker: 'Нужен общий ответ гейта толчка, иначе толчок уйдёт дважды' });
  push(4, TITLES[3]!, { state: 'waiting' });
  push(5, TITLES[4]!, { state: 'in_progress', executor_id: child, attempts: 1 });
  push(6, 'Сторож лимитов на машине', { state: 'in_progress' });
  push(7, 'Миграция настроек в хаб', { state: 'waiting' });
  push(8, 'Скриншоты для README', { state: 'waiting' });

  const feed = new Feed(api, now - 50 * MINUTE);
  const run = api.id();
  const fail = { summary: 'error' } as const;
  feed.input('Разложи карточки плана на задачи и раздай делегатам');
  feed.text(run, 'Завожу план.');
  feed.call(run, 'add', 'mcp__canterlot__plan_add', { tasks: TITLES.map((title) => ({ title })) }, hubText({ numbers: [1, 2, 3, 4, 5], tasks: [1, 2, 3, 4, 5].map((n) => asOf(tasks[n]!, { state: 'waiting', blocker: null, findings: [], executor_id: null, attempts: 0 })) }, 'Pass plan_task to session_spawn to link a delegate; its finish moves the task to in_review and session_accept to done.'));
  feed.call(run, 'addError', 'mcp__canterlot__plan_add', { tasks: [{ title: '   ' }] }, 'invalid_arguments: title must not be empty', {}, fail);
  feed.text(run, 'Запускаю делегата на карточки.');
  feed.call(run, 'spawnPlan', 'mcp__canterlot__session_spawn', { title: 'Карточки плана', brief: 'Сделай карточки плана в ленте.', harness: 'claude_code', model: 'opus[1m]', effort: 'high', isolated: true, plan_task: 5 }, hubText({ permission_mode: 'bypass', session_id: child, status: 'started', subscribed: true, plan_task: { number: 5, linked: true }, worktree: { branch: 'canterlot/5c1d0a77', path: '/data/worktrees/5c1d0a77' } }, 'Session created. Linked to plan task #5: it moves to in_review when the session finishes.'));
  feed.call(run, 'spawnPlanError', 'mcp__canterlot__session_spawn', { title: 'Миграция', brief: 'Перенеси таблицу.', plan_task: 1 }, 'plan_task_closed: Task #1 is done; reopen it with plan_update first', {}, fail);
  feed.text(run, 'Отмечаю ход работы.');
  feed.call(run, 'updateStart', 'mcp__canterlot__plan_update', { number: 1, state: 'in_progress' }, hubText({ status: 'updated', task: asOf(tasks[1]!, { state: 'in_progress' }) }));
  feed.call(run, 'updateBlocked', 'mcp__canterlot__plan_update', { number: 3, blocker: 'Нужен общий ответ гейта толчка, иначе толчок уйдёт дважды' }, hubText({ status: 'updated', task: tasks[3] }));
  feed.call(run, 'updateReview', 'mcp__canterlot__plan_update', { number: 2, state: 'in_review', findings: tasks[2]!.findings }, hubText({ status: 'updated', task: tasks[2] }));
  feed.call(run, 'updateError', 'mcp__canterlot__plan_update', { number: 9, state: 'done' }, 'plan_task_not_found: Your plan has no task #9; plan_list shows the numbers', {}, fail);
  feed.text(run, 'Сверяюсь с планом.');
  const listed = [asOf(tasks[1]!, { state: 'in_progress' }), tasks[2]!, tasks[3]!, tasks[4]!, tasks[5]!].map((task) => ({ ...task, commits: [], commits_truncated: false, files_changed: 0, report_block_id: null }));
  feed.call(run, 'list', 'mcp__canterlot__plan_list', {}, hubText({ tasks: listed, hint: '#1 [in_progress] Таблица plan_tasks и миграция' }));
  feed.call(run, 'listError', 'mcp__canterlot__plan_list', { verbose: true }, 'invalid_arguments: unknown field `verbose`, there are no fields', {}, fail);
  feed.text(run, 'Забираю открытые задачи прошлого оркестратора.');
  feed.call(run, 'adopt', 'mcp__canterlot__plan_adopt', { from_session_id: old }, hubText({ moved: [{ from: 2, to: 6 }, { from: 3, to: 7 }, { from: 5, to: 8 }], hint: 'Moved 3 tasks.' }));
  feed.call(run, 'adoptError', 'mcp__canterlot__plan_adopt', { from_session_id: child }, "invalid_arguments: The source is a delegated session; only a root session's plan can be adopted", {}, fail);
  feed.call(run, 'addFull', 'mcp__canterlot__plan_add', { tasks: [{ title: 'Ещё одна' }] }, 'plan_full: This plan already has 200 tasks; a plan holds at most 200. Cancel or adopt into a fresh session instead.', {}, fail);
  feed.text(run, 'План разложен.');

  api.histories.set(id, feed.items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, child, old, tasks, blocks: feed.blocks };
}
