type Source = 'single' | 'merged' | 'range' | 'refetch' | 'snapshot';
type Interval = { from: bigint; to: bigint; how: Source };

export class Ledger {
  private ranges = new Map<string, Interval[]>();
  constructor(private enabled = true) {}
  reset(scope: string): void { if (this.enabled) this.ranges.delete(scope); }
  record(scope: string, from: string, to: string, how: Source): void {
    if (!this.enabled) return;
    const range = { from: BigInt(from), to: BigInt(to), how };
    if (range.to < range.from) return;
    const list = this.ranges.get(scope) ?? [];
    list.push(range); this.ranges.set(scope, list);
  }
  verify(scope: string, head: string): { ok: boolean; holes: [string, string][]; doubled: [string, string][] } {
    const end = BigInt(head);
    const points = new Map<bigint, number>([[1n, 0], [end + 1n, 0]]);
    for (const r of this.ranges.get(scope) ?? []) {
      if (r.to < 1n || r.from > end) continue;
      const a = r.from < 1n ? 1n : r.from;
      const b = r.to > end ? end : r.to;
      points.set(a, (points.get(a) ?? 0) + 1); points.set(b + 1n, (points.get(b + 1n) ?? 0) - 1);
    }
    const sorted = [...points.keys()].sort((a, b) => a < b ? -1 : a > b ? 1 : 0);
    const holes: [string, string][] = [], doubled: [string, string][] = [];
    let count = 0;
    for (let i = 0; i < sorted.length - 1; i++) {
      const a = sorted[i]!, b = sorted[i + 1]! - 1n;
      count += points.get(a)!;
      if (a <= b && count !== 1) {
        const target = count === 0 ? holes : doubled;
        const last = target.at(-1);
        if (last && BigInt(last[1]) + 1n === a) last[1] = b.toString();
        else target.push([a.toString(), b.toString()]);
      }
    }
    return { ok: !holes.length && !doubled.length, holes, doubled };
  }
}

export function createProbe(enabled = false) {
  const ledger = new Ledger(enabled);
  const marks = new Map<string, number>(), metrics = new Map<string, number[]>(), counters = new Map<string, number>();
  return {
    enabled, ledger,
    mark(name: string): void { if (enabled) marks.set(name, performance.now()); },
    measure(name: string, from: string, to?: string): number | null {
      if (!enabled || !marks.has(from)) return null;
      const value = (to ? marks.get(to) : performance.now());
      if (value === undefined) return null;
      const result = value - marks.get(from)!; this.record(name, result); return result;
    },
    count(name: string, n = 1): void { if (enabled) counters.set(name, (counters.get(name) ?? 0) + n); },
    record(metric: string, value: number): void {
      if (!enabled) return;
      const values = metrics.get(metric) ?? [];
      if (values.length === 10_000) values.shift();
      values.push(value); metrics.set(metric, values);
    },
    snapshot(): unknown { return { counters: Object.fromEntries(counters), metrics: Object.fromEntries([...metrics].map(([k, v]) => [k, [...v]])) }; }
  };
}
function optIn(): boolean {
  if (typeof window === 'undefined') return false;
  try { return new URLSearchParams(location.search).get('probe') === '1' || localStorage.getItem('canterlot.probe') === '1'; }
  catch { return false; }
}
export const probe = createProbe(optIn());
export type Probe = ReturnType<typeof createProbe>;
declare global { interface Window { __canterlot_probe?: Probe } }
if (probe.enabled && typeof window !== 'undefined') Object.assign(window, { __canterlot_probe: probe });
