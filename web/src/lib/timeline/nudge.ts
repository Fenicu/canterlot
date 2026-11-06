// Подписи автопинка: состояние правила и сигналы человеческим русским.
// Английский текст толчка остаётся в ленте, но строка хаба строится из структуры.
import type { InputOrigin } from '#lib/stores/inputs.js';

export type NudgeRuleLike = {
  enabled?: boolean;
  state?: string;
  idle_minutes?: number;
  total?: number;
  max_total?: number;
};

type NudgeOrigin = InputOrigin & {
  attempt?: number;
  max?: number;
  signals?: Record<string, unknown> | null;
  event?: (NonNullable<InputOrigin['event']> & {
    signals?: Record<string, unknown> | null;
    nudges?: number;
    nudge_exhausted?: boolean;
  }) | null;
};

const STATE: Record<string, string> = {
  watching: 'следит',
  pending: 'ждёт толчка',
  exhausted: 'исчерпан',
  paused: 'на паузе',
};

/** Состояние правила одной строкой; выключенное правило выглядит как пауза. */
export function nudgeStateText(rule: NudgeRuleLike | null | undefined): string {
  if (!rule) return '';
  if (rule.enabled === false) return 'Автопинок: на паузе';
  const state = rule.state ?? 'watching';
  if (state === 'pending') {
    return `Автопинок: ждёт толчка через ${rule.idle_minutes ?? 3} мин`;
  }
  return `Автопинок: ${STATE[state] ?? 'следит'}`;
}

/** Счётчики правила: сколько толчков уже ушло из предела. */
export function nudgeCounter(rule: NudgeRuleLike | null | undefined): string {
  return `толчков ${rule?.total ?? 0} из ${rule?.max_total ?? 20}`;
}

const SIGNALS: [string, string][] = [
  ['no_marker', 'нет маркера готовности'],
  ['continue_phrase', 'обещал продолжить'],
  ['uncommitted', 'незакоммиченные изменения'],
  ['background_suspected', 'фоновая работа'],
  ['short_turn', 'короткий ход'],
  ['context_high', 'контекст заполнен'],
  ['question_in_text', 'спросил текстом'],
  ['report_missing', 'отчёт ещё не появился'],
  ['report_probe_failed', 'отчёт не удалось проверить'],
];

/** Русские подписи сигналов в устойчивом порядке; неизвестные ключи не печатаются. */
export function signalLabels(signals: unknown): string[] {
  if (!signals || typeof signals !== 'object') return [];
  const object = signals as Record<string, unknown>;
  return SIGNALS.filter(([key]) => object[key] === true).map(([, label]) => label);
}

function nudgeSignals(origin: NudgeOrigin): unknown {
  return origin.signals ?? origin.event?.signals ?? null;
}

/** Тихая строка толчка: номер попытки и первые два сигнала. */
export function nudgeEntryLine(
  origin: InputOrigin | null | undefined,
): string | null {
  if (origin?.kind !== 'nudge') return null;
  const value = origin as NudgeOrigin;
  if (typeof value.attempt !== 'number') return 'Автопинок: толчок';
  const attempt =
    typeof value.max === 'number'
      ? `толчок ${value.attempt} из ${value.max}`
      : `толчок ${value.attempt}`;
  const labels = signalLabels(nudgeSignals(value)).slice(0, 2);
  return labels.length > 0 ? `Автопинок: ${attempt} · ${labels.join(', ')}` : `Автопинок: ${attempt}`;
}

/** Строка исчерпания в карточке итога: правило сдалось после серии толчков. */
export function nudgeExhaustedLine(event: unknown): string | null {
  if (!event || typeof event !== 'object') return null;
  const value = event as { nudges?: unknown; nudge_exhausted?: unknown };
  if (value.nudge_exhausted !== true) return null;
  const count = typeof value.nudges === 'number' ? value.nudges : 0;
  return `Автопинок сдался после ${count} толчков без прогресса`;
}
