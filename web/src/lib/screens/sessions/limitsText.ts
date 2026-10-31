import { providerLabel as label } from "#lib/format/labels.js";
import { fmt } from "#lib/format/ru.js";
import {
  limitView,
  NO_LIMIT_DATA,
  type LimitThresholds,
  type ProviderLimits,
} from "#lib/stores/limits.js";

export interface LimitRow {
  label: string;
  fiveHour: string;
  weekly: string;
  hot: { fiveHour: boolean; weekly: boolean };
  /** Хаб не знает ни одного окна: строка сводится к «нет данных». */
  empty: boolean;
  /** Провайдер на недельном резерве: это дополнение к цифрам, а не замена им. */
  reserve: boolean;
}

export interface LimitStrip {
  summary: string;
  rows: LimitRow[];
  details: string;
  tone: "default" | "warning" | "reserve";
  staleNote: string;
}

export function limitStrip(
  providers: ProviderLimits[],
  now: number,
  cfg: LimitThresholds,
  opts: { prefer?: string } = {},
): LimitStrip {
  const ordered = [...providers].sort(
    (a, b) => rank(a.provider, opts.prefer) - rank(b.provider, opts.prefer),
  );
  const views = ordered.map((provider) => ({
    provider,
    view: limitView(provider, now, cfg),
  }));
  const summary = views
    .map(({ provider, view }) =>
      empty(provider)
        ? `${label(provider.provider)} · ${NO_LIMIT_DATA}`
        : `${label(provider.provider)} 5ч ${view.fiveHour} · неделя ${view.weekly}`,
    )
    .join("  ");
  const details = views
    .map(({ provider }) => {
      if (empty(provider)) return `${label(provider.provider)}: ${NO_LIMIT_DATA}`;
      const five = provider.five_hour?.resets_at
        ? fmt.clock(Date.parse(provider.five_hour.resets_at))
        : "";
      const weekAt = provider.weekly?.resets_at
        ? Date.parse(provider.weekly.resets_at)
        : Number.NaN;
      const week = Number.isFinite(weekAt)
        ? `${weekday(weekAt)}, ${fmt.clock(weekAt)}`
        : "";
      const fivePart = provider.five_hour ? `5ч до ${five}` : `5ч ${NO_LIMIT_DATA}`;
      const weekPart = provider.weekly ? `неделя до ${week}` : `неделя ${NO_LIMIT_DATA}`;
      return `${label(provider.provider)}: ${fivePart} · ${weekPart}`;
    })
    .join("\n");
  let tone: LimitStrip["tone"] = "default";
  if (views.some((item) => item.view.tone === "reserve")) tone = "reserve";
  else if (views.some((item) => item.view.tone === "warning")) tone = "warning";
  const staleAt = views
    .map((item) => observed(item.provider))
    .filter(
      (stamp) =>
        Number.isFinite(stamp) && now - stamp > cfg.stale_after_secs * 1000,
    )
    .sort((a, b) => a - b)[0];
  return {
    summary,
    rows: views.map(({ provider, view }) => ({
      label: label(provider.provider),
      fiveHour: view.fiveHour,
      weekly: view.weekly,
      hot: view.hot,
      empty: empty(provider),
      reserve: provider.weekly_reserve_active,
    })),
    details,
    tone,
    staleNote: staleAt !== undefined ? `данные от ${fmt.clock(staleAt)}` : "",
  };
}

function empty(provider: ProviderLimits): boolean {
  return !provider.five_hour && !provider.weekly;
}

function rank(provider: string, prefer?: string): number {
  if (prefer && provider === prefer) return 0;
  if (provider === "claude") return 1;
  if (provider === "codex") return 2;
  return 3;
}

function weekday(ts: number): string {
  return new Intl.DateTimeFormat("ru-RU", { weekday: "short" })
    .format(ts)
    .replace(".", "")
    .toLocaleLowerCase("ru-RU");
}

function observed(provider: ProviderLimits): number {
  const stamps = [provider.five_hour?.observed_at, provider.weekly?.observed_at]
    .map((value) => (value ? Date.parse(value) : Number.NaN))
    .filter((value) => Number.isFinite(value));
  return stamps.length ? Math.min(...stamps) : Number.NaN;
}
