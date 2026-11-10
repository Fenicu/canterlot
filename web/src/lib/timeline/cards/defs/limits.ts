// Ответ canterlot_limits: строка на провайдера, окна в процентах расхода и время сброса.
import { remainingPct, remainingText, meterTone, type MeterTone } from '#lib/stores/limits.js';
import type { CardLogic } from '../types';

export interface LimitMeter {
  window: 'five' | 'week' | 'fable' | 'month';
  /** Расход в промилле: так считает полоса лимитов. */
  usedPermille: number;
  remaining: number;
  text: string;
  tone: MeterTone;
  resetsAt: number | null;
}

export interface LimitRow {
  provider: string;
  label: string;
  meters: LimitMeter[];
  empty: boolean;
}

export interface LimitsData {
  rows: LimitRow[];
}

const LABELS: [RegExp, string, string][] = [
  [/^Claude API budget$/i, 'claude', 'Claude'],
  [/^Codex API budget$/i, 'codex', 'Codex'],
  [/^OpenCode API budget$/i, 'opencode', 'OpenCode'],
  [/^Antigravity/i, 'antigravity', 'Antigravity']
];

const WINDOWS: Record<string, LimitMeter['window']> = { 'five-hour': 'five', 'Fable weekly': 'fable', weekly: 'week', monthly: 'month' };

function reset(text: string): number | null {
  const match = /(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}) UTC/.exec(text);
  if (!match) return null;
  const ts = Date.parse(`${match[1]}T${match[2]}:00Z`);
  return Number.isFinite(ts) ? ts : null;
}

/** Разбор текста отчёта. Незнакомая строка пропускается, а не ломает карточку. */
export function parseLimits(text: string): LimitRow[] {
  const rows: LimitRow[] = [];
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    const head = /^([^:]+):\s*(.*)$/.exec(line);
    if (!head) continue;
    const known = LABELS.find(([pattern]) => pattern.test(head[1]!.trim()));
    if (!known) continue;
    const meters: LimitMeter[] = [];
    for (const part of head[2]!.split(';')) {
      const match = /(five-hour|Fable weekly|weekly|monthly) window ([\d.]+)% used/.exec(part);
      if (!match) continue;
      const usedPermille = Math.round(Number(match[2]) * 10);
      const remaining = remainingPct(usedPermille);
      meters.push({ window: WINDOWS[match[1]!]!, usedPermille, remaining, text: remainingText(usedPermille), tone: meterTone(remaining), resetsAt: reset(part) });
    }
    rows.push({ provider: known[1], label: known[2], meters, empty: meters.length === 0 });
  }
  return rows;
}

const SHORT: Record<LimitMeter['window'], string> = { five: '5ч', week: 'нед', fable: 'Fable', month: 'мес' };

export function meterTag(window: LimitMeter['window']): string {
  return SHORT[window];
}

export const limits: CardLogic<LimitsData> = {
  label: 'Лимиты',
  parse(call) {
    if (call.state === 'running') return { rows: [] };
    if (/no limit data yet/i.test(call.output) && !/window/.test(call.output)) return { rows: [] };
    const rows = parseLimits(call.output);
    return rows.length > 0 ? { rows } : null;
  },
  summary(_call, data) {
    if (data.rows.length === 0) return 'Лимиты: данных пока нет';
    const parts = data.rows.map((row) =>
      row.empty ? `${row.label} нет данных` : `${row.label} ${row.meters.map((meter) => `${SHORT[meter.window]} ${meter.text}`).join(', ')}`
    );
    return `Лимиты · ${parts.join(' · ')}`;
  }
};
