import { modelLabel, providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import { resetPhrase } from '#lib/format/countdown.js';
import type { components } from '#lib/api/generated/openapi.js';
import { dayClock, remainingPct, remainingText, shortWeekday, type ForecastView, type LimitThresholds, type MeterTone, type MeterView, type ProviderLimits } from '#lib/stores/limits.js';
import { limitStrip, providerRank } from '#lib/screens/sessions/limitsText.js';
type Series = components['schemas']['ProviderSeries'];
type Usage = components['schemas']['UsageDay'];

export function localDay(date: Date, zone = Intl.DateTimeFormat().resolvedOptions().timeZone): string {
  const parts = new Intl.DateTimeFormat('en-CA', { timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit' }).formatToParts(date);
  return ['year', 'month', 'day'].map(key => parts.find(p => p.type === key)!.value).join('-');
}
export function sevenDays(now: Date): string[] {
  return Array.from({ length: 7 }, (_, index) => { const day = new Date(now); day.setDate(day.getDate() - 6 + index); return localDay(day); });
}
type WindowKey = 'five' | 'week' | 'fable' | 'month';
const WINDOW_LABEL: Record<WindowKey, string> = { five: '5 часов', week: 'неделя', fable: 'Fable, неделя', month: 'месяц' };

export interface WindowRow {
  key: WindowKey;
  label: string;
  /** Остаток для полоски, 0..100. */
  fill: number;
  /** «осталось 39%» или «устарело»: слово прямо при числе, чтобы не гадать, осталось или потрачено. */
  value: string;
  tone: MeterTone;
  stale: boolean;
  /** «сброс через 2 ч 15 мин»; у устаревшего окна пусто. */
  reset: string;
  /** Момент сброса для подсказки с полной датой; `NaN` без сброса. */
  resetAt: number;
  /** То же словами для чтения вслух. */
  resetLong: string;
  /** Прогноз: предупреждение или спокойная строка со скоростью; `null` без поля хаба. */
  forecast: ForecastView | null;
  /** Остаток без слова: «4%», «<1%» или «устарело». */
  pct: string;
}

/** Самое тесное свежее окно провайдера: его остаток стоит крупно в шапке карточки. */
export interface LeadWindow {
  key: WindowKey;
  pct: string;
  caption: string;
  tone: MeterTone;
}

const LEAD_CAPTION: Record<WindowKey, string> = { five: 'осталось на 5 часов', week: 'осталось на неделю', fable: 'осталось на неделю Fable', month: 'осталось на месяц' };
const WINDOW_SPAN: Record<WindowKey, number> = { five: 0, week: 1, fable: 1, month: 2 };
/** Тон словом, а не только цветом: красное и жёлтое различимы и без цвета. */
const TONE_WORD: Record<MeterTone, string> = { normal: '', warning: 'заканчивается', danger: 'на исходе' };

function leadWindow(windows: WindowRow[]): LeadWindow | null {
  const fresh = windows.filter(row => !row.stale);
  if (!fresh.length) return null;
  // При равном остатке тесней длинное окно: его не переждать.
  const lead = fresh.reduce((best, row) => row.fill < best.fill || (row.fill === best.fill && WINDOW_SPAN[row.key] > WINDOW_SPAN[best.key]) ? row : best);
  return { key: lead.key, pct: lead.pct, caption: LEAD_CAPTION[lead.key], tone: lead.tone };
}

/** Текущие лимиты по провайдерам: строка на каждое окно, которое у провайдера есть. */
export function currentBlocks(providers: ProviderLimits[], now: number, cfg: LimitThresholds) {
  return limitStrip(providers, now, cfg).rows.map(row => {
    const meters: [WindowKey, MeterView | null][] = [['five', row.five], ['week', row.week], ['fable', row.fable], ['month', row.month]];
    const windows: WindowRow[] = meters
      .filter((entry): entry is [WindowKey, MeterView] => Boolean(entry[1]?.known))
      .map(([key, meter]) => ({
        key, label: WINDOW_LABEL[key], fill: meter.fill,
        value: meter.stale ? meter.text : `осталось ${meter.text}`,
        tone: meter.tone, stale: meter.stale, pct: meter.text,
        reset: resetPhrase(meter.reset), resetAt: meter.resetAt,
        resetLong: meter.resetLong ? `сброс ${meter.resetLong}` : '', forecast: meter.forecast
      }));
    const markers = [row.estimated ? 'оценка' : '', row.reserve ? 'резерв недели' : ''].filter(Boolean);
    const lead = leadWindow(windows);
    const seenAt = Math.max(...meters.flatMap(([, meter]) => meter && Number.isFinite(meter.observedAt) ? [meter.observedAt] : []));
    return {
      provider: row.provider, label: row.label, windows, empty: windows.length === 0, markers, reserve: row.reserve, lead,
      /** Все окна устарели: крупного числа нет, вместо него время последнего показания. */
      stale: windows.length > 0 && !lead,
      seen: Number.isFinite(seenAt) ? `показание ${dayClock(seenAt, now)}` : '',
      status: lead ? TONE_WORD[lead.tone] : ''
    };
  });
}

/** Недельный остаток по дням, отдельный маленький график на провайдера; 5ч в истории не показывается. */
export function historyCharts(series: Series[], days: string[], zone: string) {
  const seen = new Set<string>();
  return series
    .filter(item => !seen.has(item.provider) && seen.add(item.provider))
    .sort((a, b) => providerRank(a.provider) - providerRank(b.provider))
    .map(item => {
      // Снимок дня — последнее показание за этот день.
      const last = new Map<string, number>();
      for (const point of [...item.weekly].sort((a, b) => Date.parse(a.observed_at) - Date.parse(b.observed_at))) last.set(localDay(new Date(point.observed_at), zone), point.used_permille);
      const points = days.map(day => {
        const used = last.get(day);
        return { day, mark: String(Number(day.slice(-2))), weekday: shortWeekday(new Date(`${day}T12:00:00`).getTime()), value: used === undefined ? null : remainingPct(used), text: used === undefined ? '' : remainingText(used) };
      });
      const known = points.flatMap(p => p.value === null ? [] : [p.value]);
      const pct = (value: number) => `${Math.round(value).toLocaleString('ru-RU')}`;
      const today = points.at(-1)?.text ?? '';
      const label = providerLabel(item.provider);
      return {
        provider: item.provider, label, days: points, today, known: known.length > 0,
        summary: `${label}: от ${pct(Math.min(...known))} до ${pct(Math.max(...known))}%, сегодня ${today || 'нет данных'}`
      };
    })
    .filter(chart => chart.known);
}
export { providerLabel as providerName } from '#lib/format/labels.js';

type Chart = ReturnType<typeof historyCharts>[number];
export type ChartSlot = Chart | { provider: string; label: string; known: false };

/**
 * Графики в порядке карточек «Сейчас»: на широком экране неделя провайдера стоит под его карточкой.
 * Провайдеру без снимков достаётся пустое место, графики провайдеров без карточки идут следом.
 */
export function alignCharts(charts: Chart[], providers: { provider: string; label: string }[]): ChartSlot[] {
  if (!charts.length) return [];
  const byProvider = new Map(charts.map(chart => [chart.provider, chart]));
  const slots: ChartSlot[] = providers.map(({ provider, label }) => byProvider.get(provider) ?? { provider, label, known: false });
  const placed = new Set(providers.map(item => item.provider));
  return [...slots, ...charts.filter(chart => !placed.has(chart.provider))];
}

/** Подзаголовок страницы перечисляет ровно те провайдеры, чьи карточки видны. */
export function limitsSubtitle(providers: { provider: string }[]): string {
  const names = [...new Set(providers.map((item) => providerLabel(item.provider)))];
  if (names.length <= 1) return names[0] ?? '';
  return `${names.slice(0, -1).join(', ')} и ${names.at(-1)}`;
}
/** Провайдеры, у которых в текущем снимке есть недельное окно. */
export function providersWithWeekly(providers: ProviderLimits[]): ReadonlySet<string> {
  return new Set(providers.filter(row => row.weekly).map(row => row.provider));
}

export function usageRows(usage: Usage[], day: string, sessions: ReadonlyMap<string, { title: string }>, weekly: ReadonlySet<string>) {
  return usage
    .filter(row => row.day === day)
    .map(row => {
      const input = row.input_tokens + row.cache_read_tokens + row.cache_write_tokens;
      return {
        id: `${row.provider}:${row.conversation_id}:${row.agent}:${row.model}`,
        conversationId: row.conversation_id,
        title: sessions.get(row.conversation_id)?.title ?? `Сессия ${row.conversation_id.slice(0, 8)}`,
        provider: row.provider,
        // Провайдер назван заголовком группы, в строке остаются модель и роль.
        detail: row.agent === 'main' ? modelLabel(row.model) : `Субагент · ${modelLabel(row.model)}`,
        input,
        output: row.output_tokens, delta: row.weekly_delta_permille,
        // Без недельного окна долю показать нечем: «≈ 0%» читалось бы как настоящее измерение.
        weekly: weekly.has(row.provider),
        tokens: input + row.output_tokens,
        // Квота измеряется за время работы, а не по каждому отдельному токену.
        approximate: true,
        overlapped: row.overlapped_runs > 0
      };
    })
    .sort((a, b) => {
      if (a.weekly !== b.weekly) return a.weekly ? -1 : 1;
      if (a.weekly && a.delta !== b.delta) return b.delta - a.delta;
      return b.tokens - a.tokens;
    });
}

type UsageRow = ReturnType<typeof usageRows>[number];

/**
 * Расход за сегодня по провайдерам: у каждой недели своя квота, поэтому доли разных провайдеров
 * не сравниваются в одном списке. Итог группы — сумма приблизительных долей её строк.
 */
export function usageGroups(rows: UsageRow[]) {
  const groups = new Map<string, UsageRow[]>();
  for (const row of rows) groups.set(row.provider, [...(groups.get(row.provider) ?? []), row]);
  return [...groups.entries()]
    .map(([provider, items]) => {
      const weekly = items[0]!.weekly;
      const total = items.reduce((sum, row) => sum + row.delta, 0);
      const top = Math.max(...items.map(row => row.delta));
      return {
        provider, label: providerLabel(provider), weekly,
        total: weekly ? fmt.pctApprox(total) : '—',
        rows: items.map(row => ({ ...row, share: weekly && top > 0 ? row.delta / top : 0 }))
      };
    })
    .sort((a, b) => (a.weekly === b.weekly ? 0 : a.weekly ? -1 : 1) || providerRank(a.provider) - providerRank(b.provider));
}

type Speed = components['schemas']['ModelSpeedSummary'];
type SpeedStats = components['schemas']['SpeedStats'];

function speedCell(stats: SpeedStats) {
  return {
    speed: stats.tokens_per_second != null ? fmt.tokensPerSecond(stats.tokens_per_second) : null,
    firstToken: stats.first_token_ms != null ? `старт ${fmt.firstToken(stats.first_token_ms)}` : null
  };
}

/** Медианы скорости по моделям: сначала быстрые за неделю, модели без чисел не показываются. */
export function speedRows(speed: Speed[]) {
  return speed
    .filter(row => row.period.tokens_per_second != null || row.period.first_token_ms != null)
    .sort((a, b) => (b.period.tokens_per_second ?? -1) - (a.period.tokens_per_second ?? -1) || a.model.localeCompare(b.model))
    .map(row => ({
      id: `${row.provider}:${row.model}`,
      model: modelLabel(row.model),
      provider: providerLabel(row.provider),
      today: speedCell(row.today),
      period: speedCell(row.period)
    }));
}

export function historyRange(now: Date) {
  const from = new Date(now); from.setHours(0, 0, 0, 0); from.setDate(from.getDate() - 6);
  const to = new Date(now); to.setHours(0, 0, 0, 0); to.setDate(to.getDate() + 1);
  return { from: from.toISOString(), to: to.toISOString() };
}
