// Список задач агента: TodoWrite у Claude Code, todowrite у OpenCode, update_plan у Codex.
import { isObject, list, parseJson, str, type JsonObject } from '../json';
import type { CardLogic } from '../types';

export type TodoState = 'pending' | 'in_progress' | 'completed' | 'cancelled';

export interface TodoEntry {
  text: string;
  status: TodoState;
  /** Пункт закрылся этим вызовом. */
  done: boolean;
  /** Пункта не было в прошлом списке. */
  added: boolean;
}

export interface TodoData {
  items: TodoEntry[];
  done: number;
  total: number;
  /** Пункты, закрытые этим вызовом. */
  closed: TodoEntry[];
  added: TodoEntry[];
  current: TodoEntry | null;
  /** Был прошлый список: разница посчитана от него. */
  compared: boolean;
  /** Вход оборван: конец списка неизвестен. */
  cut: boolean;
}

const STATES: Record<string, TodoState> = {
  pending: 'pending',
  in_progress: 'in_progress',
  inprogress: 'in_progress',
  inProgress: 'in_progress',
  completed: 'completed',
  done: 'completed',
  cancelled: 'cancelled',
  canceled: 'cancelled'
};

/** Пункты из входа вызова: `todos` у Claude Code и OpenCode, `plan` у Codex. */
export function todoItems(input: JsonObject): { text: string; status: TodoState }[] | null {
  const raw = input.todos ?? input.plan;
  if (!Array.isArray(raw)) return null;
  return list(raw)
    .filter(isObject)
    .map((item) => ({ text: (str(item.content) || str(item.step) || str(item.text)).trim(), status: STATES[str(item.status)] ?? 'pending' }))
    .filter((item) => item.text.length > 0);
}

/** Сравнение двух списков по тексту пункта: агент переписывает весь список каждым вызовом. */
export function todoDiff(now: { text: string; status: TodoState }[], before: { text: string; status: TodoState }[] | null, cut = false): TodoData {
  const previous = new Map((before ?? []).map((item) => [item.text, item.status]));
  const items: TodoEntry[] = now.map((item) => {
    const was = previous.get(item.text);
    return {
      ...item,
      done: before !== null && item.status === 'completed' && was !== undefined && was !== 'completed',
      added: before !== null && was === undefined
    };
  });
  // Отменённый пункт остаётся в списке, но работы по нему больше не будет: как в панели над вводом.
  const counted = items.filter((item) => item.status !== 'cancelled');
  return {
    items,
    done: counted.filter((item) => item.status === 'completed').length,
    total: counted.length,
    closed: items.filter((item) => item.done),
    added: items.filter((item) => item.added),
    current: items.find((item) => item.status === 'in_progress') ?? null,
    compared: before !== null,
    cut
  };
}

export const todo: CardLogic<TodoData> = {
  stays: true,
  label: 'Задачи',
  parse(call) {
    const items = todoItems(call.input);
    if (!items) return null;
    const previous = parseJson(call.previous()?.text).value;
    const before = isObject(previous) ? todoItems(previous) : null;
    return todoDiff(items, before, call.partial);
  },
  summary(_call, data) {
    const progress = `${data.done} из ${data.total}${data.cut ? '+' : ''}`;
    const parts = [`Задачи · ${progress}`];
    if (data.closed.length === 1) parts.push(`готово «${data.closed[0]!.text}»`);
    else if (data.closed.length > 1) parts.push(`готово ${data.closed.length}`);
    if (data.added.length > 0) parts.push(`новых ${data.added.length}`);
    return parts.join(' · ');
  }
};
