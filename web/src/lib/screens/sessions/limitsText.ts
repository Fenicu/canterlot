import {
  accountRank,
  accountCreditEntries,
  accountCreditLines,
  accountCredentialLines,
  accountLabel,
  accountOf,
  dayClock,
  limitKey,
  limitView,
  multiAccountProviders,
  statusBadge,
  type CredentialLine,
  type CreditEntry,
  type CreditsLine,
  type LimitThresholds,
  type StatusBadge,
  type MeterView,
  type ProviderLimits,
} from "#lib/stores/limits.js";

export interface LimitRow {
  provider: string;
  /** Учётная запись лимитов; `owner` по умолчанию. */
  account: string;
  /** Ключ строки: пара (провайдер, учётная запись), у `owner` — сам провайдер. */
  key: string;
  /** У провайдера несколько учётных записей: имя записи стоит в подписи и в `data-account`. */
  named: boolean;
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
  /** Конец входа по машинам: строка на машину с известной датой; у одной машины без её имени. */
  credentials: CredentialLine[];
  /** Сбросы лимитов по машинам; пусто — их нет или воркер о них молчит. */
  creditLines: CreditsLine[];
  /** Каждый сброс со сроком: полный экран лимитов перечисляет их поимённо. */
  creditList: CreditEntry[];
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
  const multi = multiAccountProviders(providers);
  // Записи одного провайдера идут подряд: сначала `owner`, затем остальные по имени.
  const ordered = [...providers].sort(
    (a, b) =>
      providerRank(a.provider, opts.prefer) - providerRank(b.provider, opts.prefer) ||
      accountRank(a).localeCompare(accountRank(b)),
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
    rows: views.map(({ provider, view }) => limitRow(provider, view, multi, now)),
    tone,
    updated: latest ? `обновлено ${dayClock(latest, now)}` : "",
    updatedAt: latest,
  };
}

function limitRow(
  provider: ProviderLimits,
  view: ReturnType<typeof limitView>,
  multi: ReadonlySet<string>,
  now: number,
): LimitRow {
  const label = accountLabel(provider.provider, provider.account, multi);
  return {
    provider: provider.provider,
    account: accountOf(provider),
    key: limitKey(provider.provider, provider.account),
    named: multi.has(provider.provider),
    label,
    five: view.five,
    week: view.week,
    fable: view.fable,
    month: view.month,
    empty: !provider.five_hour && !provider.weekly && !provider.weekly_fable && !provider.monthly,
    reserve: provider.weekly_reserve_active,
    estimated: provider.data_confidence === "estimated",
    status: statusBadge(provider.provider, provider.status),
    credentials: accountCredentialLines(provider, label, now),
    creditLines: accountCreditLines(provider, label, now),
    creditList: accountCreditEntries(provider),
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
