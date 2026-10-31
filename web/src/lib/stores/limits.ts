import { fmt } from "#lib/format/ru.js";

export interface LimitWindow {
  used_permille: number;
  resets_at: string | null;
  observed_at: string;
}

export interface ProviderLimits {
  provider: string;
  five_hour?: LimitWindow | null;
  weekly?: LimitWindow | null;
  weekly_reserve_active: boolean;
  weekly_reserve_resets_at?: string | null;
}

export interface LimitThresholds {
  stale_after_secs: number;
  warn_pcts: number[];
  weekly_reserve_pct: number;
}

export type MeterTone = "normal" | "warning" | "danger";

/** Остаток ниже этих долей окрашивает полосу: предупреждение и тревога. */
export const WARN_REMAINING_PCT = 20;
export const DANGER_REMAINING_PCT = 5;

export const NO_LIMIT_DATA = "нет данных";
export const STALE_DATA = "устарело";

export interface MeterView {
  /** Хаб знает это окно. */
  known: boolean;
  /** Остаток для полоски, 0..100. */
  fill: number;
  /** «46%», «<1%», «нет данных» или «устарело». */
  text: string;
  tone: MeterTone;
  /** Сброс окна уже прошёл или показание старше порога. */
  stale: boolean;
  /** «сброс 19:20» или «сброс вт 04:00»; у устаревшего окна пусто. */
  reset: string;
  observedAt: number;
}

export interface LimitView {
  five: MeterView;
  week: MeterView;
  tone: "default" | "warning" | "danger" | "reserve";
  stale: boolean;
}

export function remainingPct(usedPermille: number): number {
  return Math.min(100, Math.max(0, 100 - usedPermille / 10));
}

export function remainingText(usedPermille: number): string {
  const remaining = remainingPct(usedPermille);
  if (remaining > 0 && remaining < 1) return "<1%";
  return `${Math.round(remaining)}%`;
}

export function meterTone(remaining: number): MeterTone {
  if (remaining < DANGER_REMAINING_PCT) return "danger";
  if (remaining < WARN_REMAINING_PCT) return "warning";
  return "normal";
}

const weekdayFormat = new Intl.DateTimeFormat("ru-RU", { weekday: "short" });

export function shortWeekday(ts: number): string {
  return weekdayFormat.format(ts).replace(".", "").toLocaleLowerCase("ru-RU");
}

function sameDay(a: number, b: number): boolean {
  return new Date(a).toDateString() === new Date(b).toDateString();
}

/** Время без даты в тот же день, иначе с днём недели. */
export function dayClock(ts: number, now: number): string {
  return sameDay(ts, now)
    ? fmt.clock(ts)
    : `${shortWeekday(ts)} ${fmt.clock(ts)}`;
}

const ABSENT: MeterView = {
  known: false,
  fill: 0,
  text: NO_LIMIT_DATA,
  tone: "normal",
  stale: false,
  reset: "",
  observedAt: Number.NaN,
};

export function windowText(
  window: LimitWindow | null | undefined,
  now: number,
  options?: { staleAfterMs?: number },
): MeterView {
  if (!window) return ABSENT;
  const observedAt = Date.parse(window.observed_at);
  const staleAfterMs = options?.staleAfterMs ?? 180_000;
  const resetAt = window.resets_at ? Date.parse(window.resets_at) : Number.NaN;
  const stale =
    Number.isNaN(observedAt) ||
    now - observedAt > staleAfterMs ||
    (Number.isFinite(resetAt) && resetAt <= now);
  const remaining = remainingPct(window.used_permille);
  return {
    known: true,
    fill: remaining,
    text: stale ? STALE_DATA : remainingText(window.used_permille),
    tone: stale ? "normal" : meterTone(remaining),
    stale,
    reset:
      !stale && Number.isFinite(resetAt) ? `сброс ${dayClock(resetAt, now)}` : "",
    observedAt,
  };
}

export function limitView(
  provider: ProviderLimits,
  now: number,
  cfg: LimitThresholds,
): LimitView {
  const options = { staleAfterMs: cfg.stale_after_secs * 1000 };
  const five = windowText(provider.five_hour, now, options);
  const week = windowText(provider.weekly, now, options);
  let tone: LimitView["tone"] = "default";
  if (provider.weekly_reserve_active) tone = "reserve";
  else if (five.tone === "danger" || week.tone === "danger") tone = "danger";
  else if (five.tone === "warning" || week.tone === "warning") tone = "warning";
  return { five, week, tone, stale: five.stale || week.stale };
}
