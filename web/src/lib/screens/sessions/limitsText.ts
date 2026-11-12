import { providerLabel as label } from "#lib/format/labels.js";
import {
  dayClock,
  limitView,
  statusBadge,
  type LimitThresholds,
  type StatusBadge,
  type MeterView,
  type ProviderLimits,
} from "#lib/stores/limits.js";

export interface LimitRow {
  provider: string;
  label: string;
  five: MeterView;
  week: MeterView;
  /** Недельная квота Fable есть только у Claude и может отсутствовать в старых данных. */
  fable: MeterView | null;
  /** Месячное окно есть не у всех провайдеров и может отсутствовать в старых данных. */
  month: MeterView | null;
  /** Хаб не знает ни одного окна: строка сводится к «нет данных». */
  empty: boolean;
  /** Провайдер на недельном резерве: это дополнение к остаткам, а не замена им. */
  reserve: boolean;
  /** Показание выведено из локальных журналов, а не получено от провайдера. */
  estimated: boolean;
  /** Статус провайдера словом, когда он не в порядке; иначе `null`. */
  status: StatusBadge | null;
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
    (a, b) => providerRank(a.provider, opts.prefer) - providerRank(b.provider, opts.prefer),
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
    .flatMap(({ view }) => [view.five.observedAt, view.week.observedAt, view.fable?.observedAt ?? Number.NaN])
    .filter((stamp) => Number.isFinite(stamp));
  const latest = seen.length ? Math.max(...seen) : 0;
  return {
    rows: views.map(({ provider, view }) => ({
      provider: provider.provider,
      label: label(provider.provider),
      five: view.five,
      week: view.week,
      fable: view.fable,
      month: view.month,
      empty: !provider.five_hour && !provider.weekly && !provider.weekly_fable && !provider.monthly,
      reserve: provider.weekly_reserve_active,
      estimated: provider.data_confidence === "estimated",
      status: statusBadge(provider.provider, provider.status),
    })),
    tone,
    updated: latest ? `обновлено ${dayClock(latest, now)}` : "",
    updatedAt: latest,
  };
}

/** Порядок провайдеров: выбранный, затем Claude, Codex и остальные. */
export function providerRank(provider: string, prefer?: string): number {
  if (prefer && provider === prefer) return 0;
  if (provider === "claude") return 1;
  if (provider === "codex") return 2;
  return 3;
}

/** Плашка свёрнутой полосы: одно окно провайдера, у которого остаток меньше всего. */
export interface LimitPlaque {
  tag: "5ч" | "нед" | "мес";
  meter: MeterView;
}

export function limitPlaque(row: LimitRow): LimitPlaque | null {
  if (row.empty) return null;
  const windows: LimitPlaque[] = [
    ...(row.five.absent ? [] : [{ tag: "5ч" as const, meter: row.five }]),
    { tag: "нед" as const, meter: row.week },
    ...(row.month ? [{ tag: "мес" as const, meter: row.month }] : []),
  ];
  const live = windows.filter((window) => window.meter.known && !window.meter.stale);
  if (live.length === 0) return windows.find((window) => window.meter.known) ?? windows[0] ?? null;
  // При равном остатке важнее длинное окно: его не переждать.
  return live.reduce((best, window) => (window.meter.fill <= best.meter.fill ? window : best));
}
