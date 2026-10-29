import os from 'node:os';

export interface Distribution {
  metric: string;
  n: number;
  durationMs: number;
  min: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  target: string;
  pass: boolean;
  conditions: Record<string, string | number>;
}

export function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  if (sorted.length === 1) return sorted[0];
  const rank = (p / 100) * (sorted.length - 1);
  const lower = Math.floor(rank);
  const upper = Math.ceil(rank);
  const weight = rank - lower;
  return sorted[lower] * (1 - weight) + sorted[upper] * weight;
}

export function distribution(
  samples: number[],
  meta: Omit<Distribution, 'n' | 'min' | 'p50' | 'p95' | 'p99' | 'max' | 'pass'> & {
    limitMs: { p95: number; p99?: number };
  }
): Distribution {
  const sorted = [...samples].sort((a, b) => a - b);
  const n = sorted.length;
  const min = n > 0 ? sorted[0] : 0;
  const max = n > 0 ? sorted[n - 1] : 0;
  const p50 = percentile(sorted, 50);
  const p95 = percentile(sorted, 95);
  const p99 = percentile(sorted, 99);
  const pass = p95 <= meta.limitMs.p95 && (meta.limitMs.p99 === undefined || p99 <= meta.limitMs.p99);
  const { limitMs: _limitMs, ...rest } = meta;
  void _limitMs;
  return {
    ...rest,
    n,
    min: Number(min.toFixed(2)),
    p50: Number(p50.toFixed(2)),
    p95: Number(p95.toFixed(2)),
    p99: Number(p99.toFixed(2)),
    max: Number(max.toFixed(2)),
    pass,
  };
}

export function standardConditions(extra: Record<string, string | number> = {}): Record<string, string | number> {
  const load = os.loadavg();
  return {
    platform: `${os.platform()} ${os.arch()}`,
    cpus: os.cpus().length,
    loadavg_1m_at_end: Number(load[0].toFixed(2)),
    loadavg_5m_at_end: Number(load[1].toFixed(2)),
    memory_free_gb: Number((os.freemem() / (1024 ** 3)).toFixed(2)),
    node_version: process.version,
    ...extra
  };
}
