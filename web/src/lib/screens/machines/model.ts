import type { components } from '$lib/api/generated/openapi';

export function machineStatus(value: string): string {
  return ({ available: 'В сети', ready: 'В сети', degraded: 'Связь нестабильна', unavailable: 'Не в сети' })[value] ?? 'Не в сети';
}
export function machineWarnings(values: string[]): string[] {
  const labels: Record<string, string> = { disk_usage_high: 'Мало свободного места на диске', metrics_stale: 'Данные устарели', limits_mismatch: 'Лимиты машины не совпадают с настройками хаба' };
  return values.flatMap(value => labels[value] ? [labels[value]!] : []);
}
export function diskView(disk: components['schemas']['DiskView'], roots: string[], threshold: number) {
  const index = /^root_(\d+)$/.exec(disk.label)?.[1];
  const label = disk.label === 'data' ? 'Данные воркера' : index ? roots[Number(index)] ?? `Корень ${Number(index) + 1}` : 'Диск';
  const pct = disk.total_bytes > 0 ? Math.max(0, Math.min(100, (disk.total_bytes - disk.free_bytes) * 100 / disk.total_bytes)) : null;
  return { label, pct, warn: pct !== null && pct >= threshold };
}
