import type { components } from '#lib/api/generated/openapi.js';

export { availabilityLabel as machineStatus } from '#lib/format/labels.js';
export function machineWarnings(values: string[]): string[] {
  const labels: Record<string, string> = { disk_usage_high: 'Мало свободного места на диске', metrics_stale: 'Данные устарели', limits_mismatch: 'Лимиты машины не совпадают с настройками хаба', cj_journal_stopped: 'Хаб не принимает журнал машины: все её сессии и команды стоят, пока не обновлён хаб или воркер', sj_journal_stopped: 'Хаб не принимает журнал сессии: её события не доставляются, пока не обновлён хаб или воркер' };
  return values.flatMap(value => labels[value] ? [labels[value]!] : []);
}
export function journalStops(values: string[]): string[] {
  return machineWarnings(values.filter(value => value.endsWith('_journal_stopped')));
}
export function diskView(disk: components['schemas']['DiskView'], roots: string[], threshold: number) {
  const index = /^root_(\d+)$/.exec(disk.label)?.[1];
  const label = disk.label === 'data' ? 'Данные воркера' : index ? roots[Number(index)] ?? `Корень ${Number(index) + 1}` : 'Диск';
  const pct = disk.total_bytes > 0 ? Math.max(0, Math.min(100, (disk.total_bytes - disk.free_bytes) * 100 / disk.total_bytes)) : null;
  return { label, pct, warn: pct !== null && pct >= threshold };
}
