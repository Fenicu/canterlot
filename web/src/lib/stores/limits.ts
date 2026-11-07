import { fmt } from "#lib/format/ru.js";
import { spanText, untilLong, untilShort, untilText } from "#lib/format/countdown.js";

/** Прогноз хаба по средней скорости расхода окна; нет поля — данных для прогноза мало. */
export interface WindowForecast {
  rate_permille_per_hour: number;
  recent_rate_permille_per_hour?: number | null;
  exhausts_at?: string | null;
  before_reset: boolean;
}

export interface LimitWindow {
  used_permille: number;
  resets_at: string | null;
  observed_at: string;
  forecast?: WindowForecast | null;
}

export interface ProviderLimits {
  provider: string;
  five_hour?: LimitWindow | null;
  weekly?: LimitWindow | null;
  monthly?: LimitWindow | null;
  weekly_reserve_active: boolean;
  weekly_reserve_resets_at?: string | null;
  /** У провайдера нет пятичасового окна (план без этого лимита): счётчик не показывается. */
  five_hour_absent?: boolean;
  /** `estimated`, когда CodexBar вывел расход из локальных журналов, а не от провайдера. */
  data_confidence?: "exact" | "estimated";
}

export interface LimitThresholds {
  stale_after_secs: number;
  /** Устарело: первый порог красил полосу до появления остатка в конфиге. */
  warn_pcts: number[];
  /** Действующий резерв как расход, %: совместимо с прежним полем. */
  weekly_reserve_pct: number;
  /** Жёлтый, когда остаток меньше этого процента. */
  warn_remaining_pct: number;
  /** Красный, когда остаток меньше этого процента. */
  danger_remaining_pct: number;
  /** Сколько недельного остатка оставить себе; `0` — без резерва. */
  reserve_remaining_pct: number;
}

export type MeterTone = "normal" | "warning" | "danger";

/** Умолчания, пока конфиг хаба не загружен: те же значения, что у `/api/v1/config`. */
export const DEFAULT_LIMIT_THRESHOLDS: LimitThresholds = {
  stale_after_secs: 2100,
  warn_pcts: [80, 90, 95],
  weekly_reserve_pct: 95,
  warn_remaining_pct: 20,
  danger_remaining_pct: 5,
  reserve_remaining_pct: 5,
};

/** Остаток ниже этих долей окрашивает полосу: предупреждение и тревога. */
export const WARN_REMAINING_PCT = DEFAULT_LIMIT_THRESHOLDS.warn_remaining_pct;
export const DANGER_REMAINING_PCT = DEFAULT_LIMIT_THRESHOLDS.danger_remaining_pct;

export const NO_LIMIT_DATA = "нет данных";
export const STALE_DATA = "устарело";

export interface MeterView {
  /** Окна у провайдера нет: счётчик не рисуется, «устарело» не ставится. */
  absent: boolean;
  /** Хаб знает это окно. */
  known: boolean;
  /** Остаток для полоски, 0..100. */
  fill: number;
  /** «46%», «<1%», «нет данных» или «устарело». */
  text: string;
  tone: MeterTone;
  /** Сброс окна уже прошёл или показание старше порога. */
  stale: boolean;
  /** «через 2 ч 15 мин» до сброса; у устаревшего окна и после сброса пусто. */
  reset: string;
  /** Без «через» для узкой раскрытой полосы: «2 ч 15 мин». */
  resetSpan: string;
  /** То же сжато для свёрнутой полосы: «2ч15м». */
  resetShort: string;
  /** То же словами для чтения вслух: «через 2 часа 15 минут». */
  resetLong: string;
  /** Момент сброса для подсказки с полной датой; `NaN`, когда сброс не показывается. */
  resetAt: number;
  /** Прогноз расхода; `null`, когда хаб его не прислал или окно устарело. */
  forecast: ForecastView | null;
  observedAt: number;
}

export interface ForecastView {
  /** Окно кончится раньше сброса: предупреждение. */
  before: boolean;
  /** «кончится через 3 ч» или «хватит до сброса». */
  text: string;
  /** То же словами: «кончится через 3 часа». */
  long: string;
  /** Средняя скорость окна: «≈4 %/ч». */
  pace: string;
}

/** Скорость расхода в процентах окна за час: целые от процента, десятые ниже. */
export function paceText(permillePerHour: number): string {
  const pct = Math.max(0, permillePerHour) / 10;
  const value = pct >= 1 || pct === 0 ? String(Math.round(pct)) : pct.toFixed(1).replace(".", ",");
  return `≈${value}\u00a0%/ч`;
}

/** Прогноз окна по полю хаба: предупреждение только при `before_reset`, иначе спокойная строка. */
export function forecastView(
  forecast: WindowForecast | null | undefined,
  usedPermille: number,
  now: number,
): ForecastView | null {
  if (!forecast || usedPermille >= 1000) return null;
  const pace = paceText(forecast.rate_permille_per_hour);
  const exhaustsAt = forecast.exhausts_at ? Date.parse(forecast.exhausts_at) : Number.NaN;
  if (!forecast.before_reset || !Number.isFinite(exhaustsAt)) {
    return { before: false, text: "хватит до сброса", long: "хватит до сброса", pace };
  }
  const left = exhaustsAt - now;
  if (left < 60_000) {
    return { before: true, text: "кончится с минуты на минуту", long: "кончится с минуты на минуту", pace };
  }
  return { before: true, text: `кончится ${untilText(left, true)}`, long: `кончится ${untilLong(left, true)}`, pace };
}

export interface LimitView {
  five: MeterView;
  week: MeterView;
  /** Месячное окно есть не у всех провайдеров и может отсутствовать в старых данных. */
  month: MeterView | null;
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

export function meterTone(
  remaining: number,
  thresholds: Pick<LimitThresholds, "warn_remaining_pct" | "danger_remaining_pct"> = DEFAULT_LIMIT_THRESHOLDS,
): MeterTone {
  if (remaining < thresholds.danger_remaining_pct) return "danger";
  if (remaining < thresholds.warn_remaining_pct) return "warning";
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

const NOT_SEEN: MeterView = {
  absent: false,
  known: false,
  fill: 0,
  text: NO_LIMIT_DATA,
  tone: "normal",
  stale: false,
  reset: "",
  resetSpan: "",
  resetShort: "",
  resetLong: "",
  resetAt: Number.NaN,
  forecast: null,
  observedAt: Number.NaN,
};

export function windowText(
  window: LimitWindow | null | undefined,
  now: number,
  options?: {
    staleAfterMs?: number;
    thresholds?: Pick<LimitThresholds, "warn_remaining_pct" | "danger_remaining_pct">;
  },
): MeterView {
  if (!window) return NOT_SEEN;
  const observedAt = Date.parse(window.observed_at);
  const staleAfterMs = options?.staleAfterMs ?? 180_000;
  const resetAt = window.resets_at ? Date.parse(window.resets_at) : Number.NaN;
  const stale =
    Number.isNaN(observedAt) ||
    now - observedAt > staleAfterMs ||
    (Number.isFinite(resetAt) && resetAt <= now);
  const remaining = remainingPct(window.used_permille);
  const resetShown = !stale && Number.isFinite(resetAt);
  const left = resetAt - now;
  return {
    absent: false,
    known: true,
    fill: remaining,
    text: stale ? STALE_DATA : remainingText(window.used_permille),
    tone: stale ? "normal" : meterTone(remaining, options?.thresholds),
    stale,
    reset: resetShown ? untilText(left) : "",
    resetSpan: resetShown ? spanText(left) : "",
    resetShort: resetShown ? untilShort(left) : "",
    resetLong: resetShown ? untilLong(left) : "",
    resetAt: resetShown ? resetAt : Number.NaN,
    forecast: stale ? null : forecastView(window.forecast, window.used_permille, now),
    observedAt,
  };
}

export function limitView(
  provider: ProviderLimits,
  now: number,
  cfg: LimitThresholds,
): LimitView {
  const options = {
    staleAfterMs: cfg.stale_after_secs * 1000,
    thresholds: cfg,
  };
  const five: MeterView = provider.five_hour_absent && !provider.five_hour
    ? { ...NOT_SEEN, absent: true, text: "" }
    : windowText(provider.five_hour, now, options);
  const week = windowText(provider.weekly, now, options);
  const month = provider.monthly ? windowText(provider.monthly, now, options) : null;
  let tone: LimitView["tone"] = "default";
  if (provider.weekly_reserve_active) tone = "reserve";
  else if (five.tone === "danger" || week.tone === "danger" || month?.tone === "danger") tone = "danger";
  else if (five.tone === "warning" || week.tone === "warning" || month?.tone === "warning") tone = "warning";
  return { five, week, month, tone, stale: five.stale || week.stale || Boolean(month?.stale) };
}
