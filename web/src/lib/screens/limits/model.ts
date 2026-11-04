import { modelLabel, providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import type { components } from '#lib/api/generated/openapi.js';
import { remainingPct, remainingText, type LimitThresholds, type MeterTone, type MeterView, type ProviderLimits } from '#lib/stores/limits.js';
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
type WindowKey = 'five' | 'week' | 'month';
const WINDOW_LABEL: Record<WindowKey, string> = { five: '5 часов', week: 'неделя', month: 'месяц' };

export interface WindowRow {
  key: WindowKey;
  label: string;
  /** Остаток для полоски, 0..100. */
  fill: number;
  /** «осталось 39%» или «устарело»: слово прямо при числе, чтобы не гадать, осталось или потрачено. */
  value: string;
  tone: MeterTone;
  stale: boolean;
  /** «сброс чт 18:34»; у устаревшего окна пусто. */
  reset: string;
}

/** Текущие лимиты по провайдерам: строка на каждое окно, которое у провайдера есть. */
export function currentBlocks(providers: ProviderLimits[], now: number, cfg: LimitThresholds) {
  return limitStrip(providers, now, cfg).rows.map(row => {
    const meters: [WindowKey, MeterView | null][] = [['five', row.five], ['week', row.week], ['month', row.month]];
    const windows: WindowRow[] = meters
      .filter((entry): entry is [WindowKey, MeterView] => Boolean(entry[1]?.known))
      .map(([key, meter]) => ({
        key, label: WINDOW_LABEL[key], fill: meter.fill,
        value: meter.stale ? meter.text : `осталось ${meter.text}`,
        tone: meter.tone, stale: meter.stale, reset: meter.reset
      }));
    const markers = [row.estimated ? 'оценка' : '', row.reserve ? 'резерв недели' : ''].filter(Boolean);
    return { provider: row.provider, label: row.label, windows, empty: windows.length === 0, markers, reserve: row.reserve };
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
        return { day, mark: String(Number(day.slice(-2))), value: used === undefined ? null : remainingPct(used), text: used === undefined ? '' : remainingText(used) };
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

/** Подзаголовок страницы перечисляет ровно те провайдеры, чьи карточки видны. */
export function limitsSubtitle(providers: { provider: string }[]): string {
  const names = [...new Set(providers.map((item) => providerLabel(item.provider)))];
  if (names.length <= 1) return names[0] ?? '';
  return `${names.slice(0, -1).join(', ')} и ${names.at(-1)}`;
}
export function usageRows(usage: Usage[], day: string, sessions: ReadonlyMap<string, { title: string }>) {
  return usage.filter(row => row.day === day).sort((a, b) => b.weekly_delta_permille - a.weekly_delta_permille).map(row => ({
    id: `${row.provider}:${row.conversation_id}:${row.agent}:${row.model}`,
    conversationId: row.conversation_id,
    title: sessions.get(row.conversation_id)?.title ?? `Сессия ${row.conversation_id.slice(0, 8)}`,
    detail: `${row.agent === 'main' ? providerLabel(row.provider) : 'Субагент'} · ${row.model}`,
    input: row.input_tokens + row.cache_read_tokens + row.cache_write_tokens,
    output: row.output_tokens, delta: row.weekly_delta_permille,
    // Квота измеряется за время работы, а не по каждому отдельному токену.
    approximate: true,
    overlapped: row.overlapped_runs > 0
  }));
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
