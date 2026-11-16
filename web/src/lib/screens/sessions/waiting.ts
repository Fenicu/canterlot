import type { ListRow } from '#lib/stores/list.svelte.js';
import { waitFactsFromActivity } from '#lib/requests/waitCopy.js';
import { isSnoozed } from './snooze';
import type { Tab } from './grouping';

/** Раздел ожидания в списке: его же просит открыть шапка чата. */
export const WAITING_SECTION = 'waiting';

function evidence(item: ListRow): Record<string, unknown> {
  const value = item.activity.evidence;
  return value && typeof value === 'object' ? (value as Record<string, unknown>) : {};
}

function timeMs(value: unknown): number | null {
  if (typeof value !== 'string') return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** Сессия ждёт владельца: разрешение, вопрос, ссылку или подвисла. */
export function waitsForOwner(item: ListRow): boolean {
  return item.activity.state === 'waiting_for_user' || item.activity.state === 'stalled';
}

/** Начало ожидания: та же отметка, которую называет вторая строка списка. */
export function waitSince(item: ListRow): number {
  const facts = waitFactsFromActivity(item.activity);
  const since = facts?.createdAt ?? timeMs(evidence(item).last_event_at) ?? timeMs(item.activity.since);
  return since ?? Number.POSITIVE_INFINITY;
}

/**
 * Ждущие сессии вкладки, самые долгие первыми. «Закреплённые», архив,
 * корзина и поиск живут без раздела: они не про «кто ждёт меня сейчас».
 */
export function waitingRows(items: Iterable<ListRow>, tab: Tab, now = Date.now()): ListRow[] {
  if (tab.kind === 'archive' || tab.kind === 'trash' || tab.kind === 'pinned') return [];
  const rows: ListRow[] = [];
  for (const item of items) {
    // Хаб снимает «Отложить» раньше вопроса, но старая строка могла прийти со сроком.
    if (item.archived || item.trashed_at || isSnoozed(item, now) || !waitsForOwner(item)) continue;
    if (tab.kind === 'group' && !item.groups.includes(tab.id)) continue;
    rows.push(item);
  }
  return rows.sort((a, b) => waitSince(a) - waitSince(b) || a.id.localeCompare(b.id));
}

/** Сколько других сессий ждёт владельца: своя сессия в счёт шапки не идёт. */
export function otherWaitingCount(items: Iterable<ListRow>, currentId: string): number {
  let count = 0;
  for (const item of items) {
    if (item.id === currentId || item.archived || item.trashed_at) continue;
    if (waitsForOwner(item)) count += 1;
  }
  return count;
}

const FOCUS_KEY = 'canterlot.list-focus-section';

/** Просьба списку открыться на разделе: шапка чата ведёт к «Ждут вас». */
export function focusListSection(id: string): void {
  try {
    sessionStorage.setItem(FOCUS_KEY, id);
  } catch {
    // Приватный режим без хранилища: список откроется на прежнем месте.
  }
}

export function takeFocusSection(): string | null {
  try {
    const id = sessionStorage.getItem(FOCUS_KEY);
    if (id !== null) sessionStorage.removeItem(FOCUS_KEY);
    return id || null;
  } catch {
    return null;
  }
}
