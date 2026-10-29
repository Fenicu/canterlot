import type { components } from '$lib/api/generated/openapi';
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
      for (const point of [...values].sort((a, b) => Date.parse(a.observed_at) - Date.parse(b.observed_at))) map.set(localDay(new Date(point.observed_at), zone), Math.max(0, Math.min(100, point.used_permille / 10)));
      return map;
    };
    const five = points(item.five_hour); const weekly = points(item.weekly);
    return { provider: item.provider, days: days.map(day => ({ day, five: five.get(day) ?? null, weekly: weekly.get(day) ?? null })) };
  });
}
export function providerName(value: string): string { return value === 'claude' ? 'Claude' : value === 'codex' ? 'Codex' : value; }
export function usageRows(usage: Usage[], day: string, sessions: ReadonlyMap<string, { title: string }>) {
  return usage.filter(row => row.day === day).sort((a, b) => b.weekly_delta_permille - a.weekly_delta_permille).map(row => ({
    id: `${row.provider}:${row.conversation_id}:${row.agent}:${row.model}`,
    conversationId: row.conversation_id,
    title: sessions.get(row.conversation_id)?.title ?? `Сессия ${row.conversation_id.slice(0, 8)}`,
    detail: `${row.agent === 'main' ? providerName(row.provider) : 'Субагент'} · ${row.model}`,
    input: row.input_tokens + row.cache_read_tokens + row.cache_write_tokens,
    output: row.output_tokens, delta: row.weekly_delta_permille,
    // Квота измеряется за время работы, а не по каждому отдельному токену.
    approximate: true,
    overlapped: row.overlapped_runs > 0
  }));
}

export function historyRange(now: Date) {
  const from = new Date(now); from.setHours(0, 0, 0, 0); from.setDate(from.getDate() - 6);
  const to = new Date(now); to.setHours(0, 0, 0, 0); to.setDate(to.getDate() + 1);
  return { from: from.toISOString(), to: to.toISOString() };
}
