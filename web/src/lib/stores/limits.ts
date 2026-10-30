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

export interface LimitView {
  fiveHour: string;
  weekly: string;
  tone: "default" | "warning" | "reserve";
  hot: { fiveHour: boolean; weekly: boolean };
  resets: { fiveHour: string; weekly: string };
  stale: boolean;
}

export const NO_LIMIT_DATA = "нет данных";

export function windowText(
  window: LimitWindow | null | undefined,
  now: number,
  options?: { staleAfterMs?: number; warnPct?: number },
): { pct: string; reset: string; stale: boolean; hot: boolean } {
  if (!window)
    return { pct: NO_LIMIT_DATA, reset: "", stale: false, hot: false };
  const observed = Date.parse(window.observed_at);
  const staleAfterMs = options?.staleAfterMs ?? 180_000;
  const warnPct = options?.warnPct ?? 80;
  return {
    pct: fmt.pct(window.used_permille),
    reset: window.resets_at
      ? fmt.resetAt(Date.parse(window.resets_at), now)
      : "",
    stale: Number.isNaN(observed) || now - observed > staleAfterMs,
    hot: window.used_permille / 10 >= warnPct,
  };
}

export function limitView(
  provider: ProviderLimits,
  now: number,
  cfg: LimitThresholds,
): LimitView {
  const warn = Math.min(...cfg.warn_pcts);
  const staleMs = cfg.stale_after_secs * 1000;
  const five = windowText(provider.five_hour, now, {
    staleAfterMs: staleMs,
    warnPct: warn,
  });
  const week = windowText(provider.weekly, now, {
    staleAfterMs: staleMs,
    warnPct: warn,
  });
  let tone: LimitView["tone"] = "default";
  if (provider.weekly_reserve_active) tone = "reserve";
  else if (five.hot || week.hot) tone = "warning";
  return {
    fiveHour: five.pct,
    weekly: week.pct,
    tone,
    hot: { fiveHour: five.hot, weekly: week.hot },
    resets: { fiveHour: five.reset, weekly: week.reset },
    stale: five.stale || week.stale,
  };
}
