// Доска плана оркестратора: добавление, правка, чтение и перенос задач. Слова состояний — как на экране плана.
import { fmt } from '#lib/format/ru.js';
import { STATE_WORD, planSummary, taskTitle } from '#lib/screens/plan/planText.js';
import type { PlanTaskState } from '#lib/stores/plan.svelte.js';
import { isObject, list, num, str, strings, type Json, type JsonObject } from '../json';
import { hubError, quoted, say } from '../copy';
import type { CardCall, CardLogic } from '../types';

const STATES: readonly PlanTaskState[] = ['waiting', 'in_progress', 'in_review', 'done', 'cancelled', 'blocked'];

export function planState(value: Json | undefined): PlanTaskState | null {
  const text = str(value);
  return (STATES as readonly string[]).includes(text) ? (text as PlanTaskState) : null;
}

/** Задача, как карточке её видно из вызова: номер, название, состояние и подробности. */
export interface PlanCardTask {
  /** id задачи в хабе; у задачи из входа его нет. */
  id: string;
  number: number;
  title: string;
  state: PlanTaskState | null;
  note: string;
  blocker: string;
  findings: string[];
}

function taskOf(value: Json | undefined): PlanCardTask | null {
  if (!isObject(value)) return null;
  const number = num(value.number);
  if (number === null) return null;
  return {
    id: str(value.id),
    number,
    title: str(value.title).trim(),
    state: planState(value.state),
    note: str(value.note).trim(),
    blocker: str(value.blocker).trim(),
    findings: strings(value.findings)
  };
}

function tasksOf(value: Json | undefined): PlanCardTask[] {
  return list(value)
    .map(taskOf)
    .filter((task): task is PlanCardTask => task !== null);
}

/** «#3 Название»; без названия — только номер. */
export function planTaskName(task: Pick<PlanCardTask, 'number' | 'title'>): string {
  return task.title ? taskTitle(task) : `#${task.number}`;
}

export function tasksWord(n: number): string {
  return `${n} ${fmt.plural(n, ['задача', 'задачи', 'задач'])}`;
}

/* ------------------------------------------------------------------ ошибки */

export interface PlanError {
  code: string;
  text: string;
}

function numberIn(message: string): string {
  return /#(\d+)/.exec(message)?.[1] ?? '';
}

const INVALID: [RegExp, (match: RegExpExecArray) => string][] = [
  [/title must not be empty/i, () => 'пустое название задачи'],
  [/title is longer than/i, () => 'название длиннее 200 знаков'],
  [/note is larger than/i, () => 'заметка больше 2 КиБ'],
  [/blocker is longer than/i, () => 'причина блокировки длиннее 500 знаков'],
  [/at most \d+ findings/i, () => 'больше 20 находок'],
  [/finding is longer than/i, () => 'находка длиннее 500 знаков'],
  [/pass 1 to (\d+) tasks/i, (match) => `за раз добавляют от 1 до ${match[1]} задач`],
  [/from_session_id is this session/i, () => 'свой же план забрать нельзя'],
  [/source is a delegated session/i, () => 'план делегата забрать нельзя'],
  [/only a root session/i, () => 'забрать план может только корневая сессия']
];

/**
 * Ошибка хаба по плану одной строкой по-русски. Код приходит первым словом ответа:
 * `plan_task_not_found: Your plan has no task #9; …`. Незнакомый код — текст хаба как есть.
 */
export function planError(output: string): PlanError {
  const line = output.split('\n').find((part) => part.trim())?.trim() ?? '';
  const match = /^([a-z_]+):\s*(.*)$/.exec(line);
  const code = match?.[1] ?? '';
  const message = match?.[2] ?? line;
  const number = numberIn(message);
  switch (code) {
    case 'plan_task_not_found':
      return { code, text: number ? `В плане нет задачи #${number}` : 'В плане нет такой задачи' };
    case 'plan_task_closed': {
      const state = planState(/ is (\w+);/.exec(message)?.[1]);
      const word = state ? `уже ${STATE_WORD[state]}` : 'уже закрыта';
      return { code, text: `Задача${number ? ` #${number}` : ''} ${word}: сначала её открывают заново` };
    }
    case 'plan_full': {
      const has = /has (\d+) tasks/.exec(message)?.[1];
      const most = /at most (\d+)/.exec(message)?.[1] ?? '200';
      return { code, text: has ? `План полон: в нём ${tasksWord(Number(has))}, вмещается ${most}` : `План полон: вмещается ${most} задач` };
    }
    case 'invalid_arguments': {
      for (const [pattern, say] of INVALID) {
        const found = pattern.exec(message);
        if (found) return { code, text: `Неверный вызов: ${say(found)}` };
      }
      return { code, text: 'Неверный вызов: аргументы не подошли хабу' };
    }
    case 'session_not_found':
      return { code, text: 'Такой сессии нет у владельца' };
    default:
      return { code, text: hubError(output) || 'Хаб не выполнил вызов' };
  }
}

function errorOf(call: CardCall): PlanError | null {
  return call.state === 'error' ? planError(call.output) : null;
}

/* ------------------------------------------------------------------ добавление */

export interface PlanAddData {
  tasks: PlanCardTask[];
  /** Задач во входе: пока ответа нет, номера неизвестны. */
  count: number;
  error: PlanError | null;
}

export const planAdd: CardLogic<PlanAddData> = {
  stays: true,
  label: 'План',
  parse(call) {
    const asked = list(call.input.tasks).filter(isObject);
    const answered = tasksOf(call.answer?.tasks);
    if (!call.hasInput && answered.length === 0 && call.state !== 'error') return null;
    const numbers = list(call.answer?.numbers).map((value) => num(value));
    // Ответ хаба несёт задачи целиком; у старого ответа могут быть одни номера.
    const tasks =
      answered.length > 0
        ? answered
        : asked.map((task, index) => ({ id: '', number: numbers[index] ?? 0, title: str(task.title).trim(), state: numbers[index] ? ('waiting' as const) : null, note: str(task.note).trim(), blocker: '', findings: [] }));
    return { tasks, count: Math.max(asked.length, tasks.length), error: errorOf(call) };
  },
  summary(call, data) {
    if (data.error) return 'План: задачи не добавлены';
    const n = data.count;
    if (call.state === 'running' || !call.output) return n > 0 ? `План: добавление ${n} ${fmt.plural(n, ['задачи', 'задач', 'задач'])}` : 'План: добавление задач';
    return `План: ${fmt.plural(n, ['добавлена', 'добавлены', 'добавлено'])} ${tasksWord(n)}`;
  }
};

/* ------------------------------------------------------------------ правка */

export type PlanEdit = 'title' | 'note' | 'note_cleared' | 'blocker_cleared' | 'findings' | 'findings_cleared';

export interface PlanUpdateData {
  number: number;
  /** Задача из ответа: после правки. */
  task: PlanCardTask | null;
  title: string;
  /** Состояние до правки, если его видно в прошлых вызовах плана этой ленты. */
  from: PlanTaskState | null;
  /** Состояние после правки: из ответа, иначе из входа. */
  to: PlanTaskState | null;
  /** Что ещё поменялось, кроме состояния. */
  edits: PlanEdit[];
  note: string;
  blocker: string;
  findings: string[];
  unchanged: boolean;
  error: PlanError | null;
}

function editsOf(input: JsonObject): PlanEdit[] {
  const out: PlanEdit[] = [];
  if (typeof input.title === 'string') out.push('title');
  if (typeof input.note === 'string') out.push(input.note.trim() ? 'note' : 'note_cleared');
  if (typeof input.blocker === 'string' && !input.blocker.trim()) out.push('blocker_cleared');
  if (Array.isArray(input.findings)) out.push(input.findings.length > 0 ? 'findings' : 'findings_cleared');
  return out;
}

const EDIT_WORD: Record<PlanEdit, string> = {
  title: 'новое название',
  note: 'заметка',
  note_cleared: 'заметка снята',
  blocker_cleared: 'блокер снят',
  findings: 'находки',
  findings_cleared: 'находки сняты'
};

export const planUpdate: CardLogic<PlanUpdateData> = {
  stays: true,
  label: 'План',
  parse(call) {
    const input = call.input;
    const task = taskOf(call.answer?.task);
    const number = task?.number ?? num(input.number);
    if (number === null) return null;
    const blocker = str(input.blocker).trim();
    const to = task?.state ?? planState(input.state) ?? (blocker ? 'blocked' : null);
    const stateAsked = planState(input.state) !== null || blocker.length > 0;
    const before = call.planBefore(number);
    return {
      number,
      task,
      title: task?.title || str(input.title).trim() || before?.title || '',
      from: before?.state ?? null,
      to: stateAsked || (before?.state && to !== before.state) ? to : null,
      edits: editsOf(input),
      note: task ? task.note : str(input.note).trim(),
      blocker: task ? task.blocker : blocker,
      findings: task ? task.findings : strings(input.findings),
      unchanged: str(call.answer?.status) === 'unchanged',
      error: errorOf(call)
    };
  },
  summary(call, data) {
    const name = planTaskName(data);
    if (data.error) return `План: задача #${data.number} не обновлена`;
    return `${name}: ${updateTail(data, call.state === 'running' || !call.output)}`;
  }
};

/** Хвост строки правки: переход состояния или что поменялось. */
export function updateTail(data: PlanUpdateData, running = false): string {
  if (data.unchanged) return 'без изменений';
  if (data.to) return data.from && data.from !== data.to ? `${STATE_WORD[data.from]} → ${STATE_WORD[data.to]}` : STATE_WORD[data.to];
  if (data.edits.length > 0) return data.edits.map((edit) => EDIT_WORD[edit]).join(', ');
  return running ? 'правка' : 'обновлена';
}

/* ------------------------------------------------------------------ чтение */

export interface PlanListData {
  tasks: PlanCardTask[];
  error: PlanError | null;
}

export const planList: CardLogic<PlanListData> = {
  stays: true,
  label: 'План',
  parse(call) {
    if (call.state === 'error') return { tasks: [], error: errorOf(call) };
    if (call.state === 'running' || !call.output) return { tasks: [], error: null };
    if (!call.answer || !Array.isArray(call.answer.tasks)) return null;
    return { tasks: tasksOf(call.answer.tasks), error: null };
  },
  summary(call, data) {
    if (data.error) return 'План: не прочитан';
    if (call.state === 'running' || !call.output) return 'План';
    const known = data.tasks.filter((task): task is PlanCardTask & { state: PlanTaskState } => task.state !== null);
    if (known.length === 0) return 'План пуст';
    return `План: ${planSummary(known)}`;
  }
};

/* ------------------------------------------------------------------ перенос */

export interface PlanAdoptData {
  from: string;
  /** Номер в старом плане → номер в своём. */
  moved: { from: number; to: number }[];
  hint: string;
  error: PlanError | null;
}

export const planAdopt: CardLogic<PlanAdoptData> = {
  stays: true,
  label: 'План',
  parse(call) {
    const from = str(call.input.from_session_id);
    if (!from && call.state !== 'error') return null;
    const moved = list(call.answer?.moved)
      .filter(isObject)
      .map((pair) => ({ from: num(pair.from) ?? 0, to: num(pair.to) ?? 0 }))
      .filter((pair) => pair.from > 0 && pair.to > 0);
    return { from, moved, hint: str(call.answer?.hint), error: errorOf(call) };
  },
  summary(call, data) {
    const title = adoptTitle(call, data.from);
    if (data.error) return say(call.voice, `Не забрала план ${title}`, `План ${title} не перенесён`);
    if (call.state === 'running' || !call.output) return say(call.voice, `Забирает план ${title}`, `Перенос плана ${title}`);
    const tail = data.moved.length > 0 ? tasksWord(data.moved.length) : 'открытых задач нет';
    return say(call.voice, `Забрала план ${title}: ${tail}`, `План ${title} перенесён: ${tail}`);
  }
};

export function adoptTitle(call: CardCall, id: string): string {
  const title = call.session(id)?.title ?? '';
  return quoted(title || (id ? `сессия ${id.slice(0, 8)}` : ''));
}

/* ------------------------------------------------------------------ запуск с задачей */

export interface SpawnPlanTask {
  number: number;
  /** Хаб связал задачу с ребёнком; `false` — задачу за время ожидания закрыли или перенесли. */
  linked: boolean | null;
}

/** Задача плана из `session_spawn`: номер из входа, исход связи из ответа. */
export function spawnPlanTask(call: CardCall): SpawnPlanTask | null {
  const answered = isObject(call.answer?.plan_task) ? call.answer.plan_task : null;
  const number = num(answered?.number) ?? num(call.input.plan_task);
  if (number === null || number < 1) return null;
  const linked = answered && typeof answered.linked === 'boolean' ? answered.linked : null;
  return { number, linked };
}
