import { modelLabel } from './labels.js';
import { fmt } from './ru.js';

/** Последнее разрешение псевдонима на машине (`ModelView.resolved` хаба). */
export interface ModelResolution {
  model: string;
  observed_at: string;
  previous?: string | null;
  changed_at?: string | null;
}

export interface CatalogModelLike {
  id: string;
  title: string;
  resolved?: ModelResolution | null;
}

/** Смена версии за псевдонимом заметна неделю: дальше она уже не новость. */
const CHANGE_FRESH_MS = 7 * 24 * 60 * 60 * 1000;
const SEEN_PREFIX = 'canterlot.model-change-seen:';

/** Пункт выбора: «Fable 1M — сейчас Fable 5.2», пока разрешение не наблюдали — просто «Fable 1M». */
export function catalogModelLabel(entry: CatalogModelLike): string {
  const resolved = entry.resolved?.model;
  if (!resolved || resolved === entry.id) return entry.title || modelLabel(entry.id);
  return `${entry.title || modelLabel(entry.id)} — сейчас ${modelLabel(resolved)}`;
}

/** Строка под выбором: во что псевдоним разрешился и когда это видели. */
export function resolutionHint(entry: CatalogModelLike | null | undefined, now: number): string {
  const resolution = entry?.resolved;
  if (!entry || !resolution || resolution.model === entry.id) return '';
  const at = Date.parse(resolution.observed_at);
  const when = Number.isFinite(at) ? ` (по ходу ${fmt.stamp(at, now)})` : '';
  return `${entry.id} → ${resolution.model}${when}. Псевдоним переходит на новую версию сам.`;
}

export interface ResolutionChange {
  key: string;
  text: string;
}

/** Псевдоним недавно стал разрешаться в другую версию: «fable[1m] теперь Fable 5.2, было Fable 5.1». */
export function resolutionChange(entry: CatalogModelLike | null | undefined, now: number): ResolutionChange | null {
  const resolution = entry?.resolved;
  if (!entry || !resolution?.previous || !resolution.changed_at) return null;
  const changed = Date.parse(resolution.changed_at);
  if (!Number.isFinite(changed) || now - changed > CHANGE_FRESH_MS) return null;
  return {
    key: `${entry.id}:${resolution.model}`,
    text: `${entry.id} теперь ${modelLabel(resolution.model)}, было ${modelLabel(resolution.previous)}`,
  };
}

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function changeSeen(key: string): boolean {
  return storage()?.getItem(SEEN_PREFIX + key) === '1';
}

/** Уведомление показывается один раз: следующее открытие его уже не повторяет. */
export function markChangeSeen(key: string): void {
  try {
    storage()?.setItem(SEEN_PREFIX + key, '1');
  } catch {
    // Хранилище недоступно (приватный режим): уведомление повторится, это не ошибка.
  }
}
