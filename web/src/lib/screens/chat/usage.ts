import { fmt } from "#lib/format/ru.js";
import type { components } from "#lib/api/generated/openapi.js";

type SessionUsage = components["schemas"]["SessionUsageView"];
type UsageTotals = components["schemas"]["UsageTotalsView"];

/** Компактное число для строки и точное — для подсказки. */
export interface UsageValue {
  compact: string;
  exact: string;
}

export interface UsageRow {
  label: string;
  value: UsageValue;
}

export interface UsageModelRow {
  model: string;
  total: UsageValue;
  input: UsageValue;
  output: UsageValue;
  cacheRead: UsageValue;
  cacheWrite: UsageValue;
}

export interface UsageSection {
  rows: UsageRow[];
  models: UsageModelRow[];
  showCacheRead: boolean;
  showCacheWrite: boolean;
  empty: boolean;
}

export interface SessionUsageSummary {
  session: UsageSection;
  delegates: UsageSection;
  hasDelegates: boolean;
}

function value(n: number): UsageValue {
  return { compact: fmt.tokensCompact(n, { short: true }), exact: fmt.tokens(n) };
}

/** Ходы — счёт, а не токены: точное число с разрядами, без «тыс.». */
function count(n: number): UsageValue {
  return { compact: fmt.number(n), exact: fmt.tokens(n) };
}

function section(totals: UsageTotals): UsageSection {
  const showCacheRead = totals.cache_read_tokens > 0;
  const showCacheWrite = totals.cache_write_tokens > 0;
  const rows: UsageRow[] = [
    { label: "Вход", value: value(totals.input_tokens) },
    { label: "Выход", value: value(totals.output_tokens) },
  ];
  if (showCacheRead)
    rows.push({ label: "Прочитано из кэша", value: value(totals.cache_read_tokens) });
  if (showCacheWrite)
    rows.push({ label: "Записано в кэш", value: value(totals.cache_write_tokens) });
  rows.push({ label: "Ходов", value: count(totals.turns) });

  const models: UsageModelRow[] = totals.models.map((row) => ({
    model: row.model,
    total: value(
      row.input_tokens +
        row.output_tokens +
        row.cache_read_tokens +
        row.cache_write_tokens,
    ),
    input: value(row.input_tokens),
    output: value(row.output_tokens),
    cacheRead: value(row.cache_read_tokens),
    cacheWrite: value(row.cache_write_tokens),
  }));

  const empty =
    totals.turns === 0 &&
    totals.input_tokens === 0 &&
    totals.output_tokens === 0 &&
    totals.cache_read_tokens === 0 &&
    totals.cache_write_tokens === 0;

  return { rows, models, showCacheRead, showCacheWrite, empty };
}

export function summarizeUsage(usage: SessionUsage): SessionUsageSummary {
  const delegates = section(usage.delegates);
  return {
    session: section(usage.session),
    delegates,
    hasDelegates: !delegates.empty,
  };
}
