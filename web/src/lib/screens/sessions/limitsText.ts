import { providerLabel as label } from "#lib/format/labels.js";
import {
  dayClock,
  limitView,
  type LimitThresholds,
  type MeterView,
  type ProviderLimits,
} from "#lib/stores/limits.js";

export interface LimitRow {
  provider: string;
  label: string;
  five: MeterView;
  week: MeterView;
  /** Месячное окно есть не у всех провайдеров и может отсутствовать в старых данных. */
  month: MeterView | null;
  /** Хаб не знает ни одного окна: строка сводится к «нет данных». */
  empty: boolean;
  /** Провайдер на недельном резерве: это дополнение к остаткам, а не замена им. */
  reserve: boolean;
  /** Показание выведено из локальных журналов, а не получено от провайдера. */
  estimated: boolean;
}

export interface LimitStrip {
  rows: LimitRow[];
  tone: "default" | "warning" | "danger" | "reserve";
  /** «обновлено 17:44»: время самого свежего показания любого окна. */
  updated: string;
  /** То же время числом для сравнения; 0, пока показаний нет. */
  updatedAt: number;
}

const TONE_RANK = { default: 0, warning: 1, danger: 2, reserve: 3 } as const;

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
  let tone: LimitStrip["tone"] = "default";
  for (const { view } of views) {
    if (TONE_RANK[view.tone] > TONE_RANK[tone]) tone = view.tone;
  }
  const seen = views
    .flatMap(({ view }) => [view.five.observedAt, view.week.observedAt])
    .filter((stamp) => Number.isFinite(stamp));
  const latest = seen.length ? Math.max(...seen) : 0;
  return {
    rows: views.map(({ provider, view }) => ({
      provider: provider.provider,
      label: label(provider.provider),
      five: view.five,
      week: view.week,
      month: view.month,
      empty: !provider.five_hour && !provider.weekly && !provider.monthly,
      reserve: provider.weekly_reserve_active,
      estimated: provider.data_confidence === "estimated",
    })),
    tone,
    updated: latest ? `обновлено ${dayClock(latest, now)}` : "",
    updatedAt: latest,
  };
}

function rank(provider: string, prefer?: string): number {
  if (prefer && provider === prefer) return 0;
  if (provider === "claude") return 1;
  if (provider === "codex") return 2;
  return 3;
}
