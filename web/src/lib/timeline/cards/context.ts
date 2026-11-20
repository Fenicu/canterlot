import { getContext, setContext } from 'svelte';
import { getRuntime } from '#lib/app/runtime.js';
import type { BlockModel } from '#lib/stores/blocks.js';
import type { TimelineEntry } from '#lib/stores/timeline.js';
import type { HubEvent } from '#lib/timeline/hubEvent.js';
import { isObject, jsonObjectIn, list, num, str } from './json';
import { planState } from './defs/plan';
import { toolId } from './toolId';
import type { PlanSnapshot, SessionFacts } from './types';

/** Что карточкам нужно знать о ленте, в которой они стоят. */
export interface FeedCards {
  /** Лента корневой сессии владельца, а не делегата и не ветки субагента. */
  root: boolean;
  /** Провайдер сессии. */
  harness: string;
  /** Предыдущий вызов с тем же id в той же дорожке; для разницы в списке задач. */
  previous(block: BlockModel): BlockModel | null;
  /** Чужая сессия из списка владельца. */
  session(id: string): SessionFacts | null;
  /** События хаба о сессии в этой ленте; пустой id — все события. */
  events(sessionId: string): HubEvent[];
  /** Задача плана с номером до этого вызова: по ответам прошлых вызовов плана в ленте. */
  planBefore?(block: BlockModel, number: number): PlanSnapshot | null;
  /** Тема записи памяти по прошлым вызовам памяти в ленте. */
  memorySubject?(block: BlockModel, id: string): string | null;
  /**
   * Открыть план оркестратора на месте: вкладка «План» в панели или листе делегирования.
   * `false` — открыть нечем, ссылка ведёт на страницу плана.
   */
  openPlan?(orchestratorId: string): boolean;
}

const KEY = Symbol('feed-cards');

/** Строка списка сессий владельца: она живая, событие хаба меняет её на месте. */
export function listSession(id: string): SessionFacts | null {
  let row;
  try {
    row = getRuntime().list.items.get(id);
  } catch {
    return null;
  }
  if (!row) return null;
  const activity = row.activity as { state?: string } | null | undefined;
  const harness = row.harness as { id?: string } | null | undefined;
  return {
    id,
    title: row.title ?? '',
    state: activity?.state ?? '',
    accepted: Boolean(row.accepted),
    harness: harness?.id ?? '',
    model: row.model ?? ''
  };
}

const NONE: FeedCards = { root: false, harness: '', previous: () => null, session: listSession, events: () => [] };

export function setFeedCards(value: FeedCards): void {
  setContext(KEY, value);
}

/** Вне ленты (экспорт картинкой, тесты компонентов) карточки говорят безлично и без истории. */
export function feedCards(): FeedCards {
  return getContext<FeedCards | undefined>(KEY) ?? NONE;
}

/**
 * Предыдущий вызов того же инструмента в той же дорожке, по записям окна ленты.
 * Дорожка — родитель блока: у субагента свой список задач.
 */
export function previousCall(entries: readonly TimelineEntry[], block: BlockModel): BlockModel | null {
  const id = toolId(block.toolName);
  let seen = false;
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index]!;
    if (entry.kind !== 'block') continue;
    const other = entry.block;
    if (other.id === block.id) {
      seen = true;
      continue;
    }
    if (!seen || other.kind !== 'tool_call' || other.parentId !== block.parentId) continue;
    if (toolId(other.toolName) === id) return other;
  }
  return null;
}

/** События хаба из вводов ленты: итог сессии, приёмка, решение владельца, запуск из очереди. */
export function feedEvents(entries: readonly TimelineEntry[], sessionId: string): HubEvent[] {
  const out: HubEvent[] = [];
  for (const entry of entries) {
    if (entry.kind !== 'input') continue;
    const event = entry.input.origin?.event;
    if (!event) continue;
    if (!sessionId || event.session_id === sessionId || event.child_id === sessionId) out.push(event);
  }
  return out;
}

/**
 * Тема записи памяти с этим id по вызовам памяти до `block`: чтение, поиск, запись и правка.
 * Ближайший к вызову назад выигрывает; записи, которых лента не видела, остаются без темы.
 */
export function memorySubject(entries: readonly TimelineEntry[], block: BlockModel, id: string): string | null {
  const results = new Map<string, BlockModel>();
  for (const entry of entries) {
    if (entry.kind === 'block' && entry.block.kind === 'tool_result' && entry.block.parentId) results.set(entry.block.parentId, entry.block);
  }
  let seen = false;
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index]!;
    if (entry.kind !== 'block') continue;
    const other = entry.block;
    if (other.id === block.id) {
      seen = true;
      continue;
    }
    if (!seen || other.kind !== 'tool_call') continue;
    const tool = toolId(other.toolName);
    if (!MEMORY_TOOLS.has(tool)) continue;
    const answer = jsonObjectIn(results.get(other.id)?.text);
    if (tool === 'canterlot.memory_search') {
      for (const row of list(answer?.results)) {
        if (isObject(row) && str(row.id) === id && str(row.subject).trim()) return str(row.subject).trim();
      }
    } else if (tool === 'canterlot.memory_get') {
      if (answer && str(answer.id) === id && str(answer.subject).trim()) return str(answer.subject).trim();
    } else {
      // Запись и правка называют тему во входе. Отказанный вызов тему не меняет: у удачного ответа
      // есть `id` записи (у отказа о конфликте версий его нет).
      const named = str(jsonObjectIn(other.text)?.subject).trim();
      if (named && str(answer?.id) === id) return named;
    }
  }
  return null;
}

const MEMORY_TOOLS = new Set(['canterlot.memory_search', 'canterlot.memory_get', 'canterlot.memory_write', 'canterlot.memory_update']);

const PLAN_TOOLS =new Set(['canterlot.plan_add', 'canterlot.plan_update', 'canterlot.plan_list', 'canterlot.plan_adopt']);

/**
 * Какой была задача плана с номером до вызова `block`: ищется назад по ответам вызовов плана
 * в окне ленты. Перенос плана перенумеровывает задачи: за ним старые номера уже не те.
 */
export function planBefore(entries: readonly TimelineEntry[], block: BlockModel, number: number): PlanSnapshot | null {
  const results = new Map<string, BlockModel>();
  for (const entry of entries) {
    if (entry.kind === 'block' && entry.block.kind === 'tool_result' && entry.block.parentId) results.set(entry.block.parentId, entry.block);
  }
  let seen = false;
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index]!;
    if (entry.kind !== 'block') continue;
    const other = entry.block;
    if (other.id === block.id) {
      seen = true;
      continue;
    }
    if (!seen || other.kind !== 'tool_call') continue;
    const id = toolId(other.toolName);
    if (!PLAN_TOOLS.has(id)) continue;
    const answer = jsonObjectIn(results.get(other.id)?.text);
    if (!answer) continue;
    if (id === 'canterlot.plan_adopt') {
      if (list(answer.moved).some((pair) => isObject(pair) && num(pair.to) === number)) return null;
      continue;
    }
    const tasks = id === 'canterlot.plan_update' ? [answer.task] : list(answer.tasks);
    for (const task of tasks) {
      if (isObject(task) && num(task.number) === number) return { title: str(task.title).trim(), state: planState(task.state) };
    }
  }
  return null;
}
