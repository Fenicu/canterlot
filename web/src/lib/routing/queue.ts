// Очередь запусков родителя: что показать плашкой, списком и бледным узлом дерева.
import { fmt } from '#lib/format/ru.js';
import type { Stage } from '#lib/ui/stage-track.js';
import { effortName, pairName, type ModelTitles } from './decision';
import { detailText, queueReasonLabel } from './reasons';

export interface QueueItem {
  id: string;
  title?: string | null;
  task_type?: string | null;
  level?: string | null;
  harness?: string | null;
  model?: string | null;
  reason: string;
  detail?: unknown;
  state: string;
  not_before: string;
  expires_at: string;
  child_id?: string | null;
  result?: unknown;
  created_at: string;
  position?: number;
}

export const QUEUE_STAGES: Stage[] = [
  { id: 'queued', label: 'В очереди', kind: 'ring' },
  { id: 'starting', label: 'Запускается', kind: 'spin' },
  { id: 'started', label: 'Создана' }
];

export const START_NOW_WARNING = 'Снимает лимиты и страж, но не белый список';

/** Ожидающие элементы: в очереди и запускающиеся. */
export function pendingItems<T extends QueueItem>(items: T[]): T[] {
  return items.filter((item) => item.state === 'queued' || item.state === 'starting');
}

function at(value: string): number {
  const ts = Date.parse(value);
  return Number.isFinite(ts) ? ts : Number.NaN;
}

/** Ближайший срок среди ожидающих; `null`, если ожидающих нет. */
export function nextStart(items: QueueItem[]): number | null {
  const times = pendingItems(items)
    .map((item) => at(item.not_before))
    .filter(Number.isFinite);
  return times.length > 0 ? Math.min(...times) : null;
}

/** «В очереди запуска: 2 · следующий в 14:32». */
export function queueSummary(items: QueueItem[], now: number = Date.now()): string {
  const count = pendingItems(items).length;
  if (count === 0) return '';
  const next = nextStart(items);
  const when = next === null ? '' : next <= now ? ' · следующий сейчас' : ` · следующий в ${fmt.clock(next)}`;
  return `В очереди запуска: ${count}${when}`;
}

/** Заголовок будущей сессии; без него хаб назовёт её сам. */
export function itemTitle(item: Pick<QueueItem, 'title'>): string {
  return item.title?.trim() || 'Запуск без названия';
}

/** Что запускается: тип задачи маршрута с уровнем или пара прямого запуска. */
export function itemTarget(
  item: Pick<QueueItem, 'task_type' | 'level' | 'harness' | 'model'>,
  taskTitle: (key: string) => string,
  titles?: ModelTitles
): string {
  if (item.task_type) {
    const level = item.level ? ` · ${effortName(item.level)}` : '';
    return `маршрут «${taskTitle(item.task_type)}»${level}`;
  }
  if (item.harness && item.model) return pairName(item.harness, item.model, titles);
  return item.harness ? pairName(item.harness, 'default', titles) : '';
}

/** Причина элемента по-русски: вид и измерения хаба. */
export function itemReason(item: Pick<QueueItem, 'reason' | 'detail'>, now: number = Date.now()): string {
  const raw = item.detail && typeof item.detail === 'object' ? (item.detail as { detail?: unknown }).detail : item.detail;
  const detail = typeof raw === 'string' ? detailText(raw, now) : '';
  const label = queueReasonLabel(item.reason);
  return detail ? `${label}: ${detail}` : label;
}

export function itemExpiry(item: QueueItem, now: number = Date.now()): string {
  const ts = at(item.expires_at);
  return Number.isFinite(ts) ? `Ждёт до ${fmt.messageTime(ts, now)}` : '';
}

/** Номер этапа дорожки «в очереди → запускается → создана». */
export function stageIndex(item: QueueItem): number {
  if (item.state === 'starting') return 1;
  if (item.state === 'started') return 2;
  return 0;
}

const ENDED: Record<string, string> = {
  cancelled: 'Отменён',
  failed: 'Не запустился',
  expired: 'Срок вышел'
};

export function endedLabel(item: QueueItem): string | null {
  return ENDED[item.state] ?? null;
}

/** Ответ действия очереди по-русски: `cancelled`, `already_started`, `started`. */
export function actionNote(status: string): string | null {
  switch (status) {
    case 'cancelled':
      return null;
    case 'started':
      return null;
    case 'already_started':
      return 'Уже запущен';
    case 'already_cancelled':
      return 'Уже отменён';
    case 'failed':
      return 'Запуск не удался';
    case 'expired':
      return 'Срок ожидания вышел';
    default:
      return status;
  }
}
