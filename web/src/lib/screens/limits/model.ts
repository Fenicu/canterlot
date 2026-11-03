import { modelLabel, providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import type { components } from '#lib/api/generated/openapi.js';
import { remainingPct } from '#lib/stores/limits.js';
type Series = components['schemas']['ProviderSeries'];
type Usage = components['schemas']['UsageDay'];

export function localDay(date: Date, zone = Intl.DateTimeFormat().resolvedOptions().timeZone): string {
  const parts = new Intl.DateTimeFormat('en-CA', { timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit' }).formatToParts(date);
  return ['year', 'month', 'day'].map(key => parts.find(p => p.type === key)!.value).join('-');
}
export function sevenDays(now: Date): string[] {
  return Array.from({ length: 7 }, (_, index) => { const day = new Date(now); day.setDate(day.getDate() - 6 + index); return localDay(day); });
}
export function chartModel(series: Series[], days: string[], zone: string) {
  return series.map(item => {
    const points = (values: Series['five_hour']) => {
      const map = new Map<string, number>();
      for (const point of [...values].sort((a, b) => Date.parse(a.observed_at) - Date.parse(b.observed_at))) map.set(localDay(new Date(point.observed_at), zone), remainingPct(point.used_permille));
      return map;
    };
    const five = points(item.five_hour); const weekly = points(item.weekly);
    return { provider: item.provider, hasFive: five.size > 0, days: days.map(day => ({ day, five: five.get(day) ?? null, weekly: weekly.get(day) ?? null })) };
  });
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
    firstToken: stats.first_token_ms != null ? `первый токен ${fmt.firstToken(stats.first_token_ms)}` : null
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
