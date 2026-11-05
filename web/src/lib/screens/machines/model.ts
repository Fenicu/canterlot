import type { components } from '#lib/api/generated/openapi.js';
import { fmt } from '#lib/format/ru.js';

export { availabilityLabel as machineStatus } from '#lib/format/labels.js';

export type DiskTone = 'normal' | 'warning' | 'danger';

export interface DiskLimits {
  warnFreeBytes: number;
  criticalFreeBytes: number;
  recoverFreeBytes: number;
}

export interface DiskConfig {
  disk_warn_free_gb: number;
  disk_critical_free_gb: number;
}

const GB = 1_000_000_000;

export function diskLimits(config: DiskConfig): DiskLimits {
  const warnFreeBytes = config.disk_warn_free_gb * GB;
  return {
    warnFreeBytes,
    criticalFreeBytes: config.disk_critical_free_gb * GB,
    // Тот же запас, что у хаба: возврат не мигает у самого порога.
    recoverFreeBytes: warnFreeBytes + 5 * GB
  };
}

export function diskTone(freeBytes: number, limits: DiskLimits): DiskTone {
  if (freeBytes <= limits.criticalFreeBytes) return 'danger';
  if (freeBytes <= limits.warnFreeBytes) return 'warning';
  return 'normal';
}

export function machineWarnings(values: string[]): string[] {
  const labels: Record<string, string> = { disk_usage_high: 'Мало свободного места на диске', metrics_stale: 'Данные устарели', limits_mismatch: 'Лимиты машины не совпадают с настройками хаба', cj_journal_stopped: 'Хаб не принимает журнал машины: все её сессии и команды стоят, пока не обновлён хаб или воркер', sj_journal_stopped: 'Хаб не принимает журнал сессии: её события не доставляются, пока не обновлён хаб или воркер' };
  return values.flatMap(value => labels[value] ? [labels[value]!] : []);
}

export function journalStops(values: string[]): string[] {
  return machineWarnings(values.filter(value => value.endsWith('_journal_stopped')));
}

export function diskView(disk: components['schemas']['DiskView'], roots: string[], limits: DiskLimits) {
  const index = /^root_(\d+)$/.exec(disk.label)?.[1];
  const label = disk.label === 'data' ? 'Данные воркера' : index ? roots[Number(index)] ?? `Корень ${Number(index) + 1}` : 'Диск';
  const pct = disk.total_bytes > 0 ? Math.max(0, Math.min(100, (disk.total_bytes - disk.free_bytes) * 100 / disk.total_bytes)) : null;
  const tone = diskTone(disk.free_bytes, limits);
  return {
    label,
    pct,
    tone,
    warn: tone === 'warning',
    text: `Свободно ${fmt.gb(disk.free_bytes)} из ${fmt.number(Math.round(disk.total_bytes / GB))}`
  };
}

/** Свободное место, на котором работает воркер: минимум по дискам отчёта. */
export function machineFreeBytes(disks: components['schemas']['DiskView'][] | undefined): number | null {
  let best: number | null = null;
  for (const disk of disks ?? []) {
    if (disk.total_bytes === 0) continue;
    if (best === null || disk.free_bytes < best) best = disk.free_bytes;
  }
  return best;
}

export interface LowDiskMachine {
  id: string;
  name: string;
  metrics: { disks?: components['schemas']['DiskView'][] } | null;
}

export interface DiskNotice {
  text: string;
  tone: 'warning' | 'danger';
}

/**
 * Одна тихая строка о машине с самым малым запасом. Уже показанная машина
 * держится до возврата выше порога с запасом, поэтому строка не мигает.
 */
export function lowDiskNotice(
  machines: LowDiskMachine[],
  limits: DiskLimits,
  active: string[]
): { notice: DiskNotice | null; active: string[] } {
  const kept = new Set<string>();
  let worst: { name: string; free: number } | null = null;
  for (const machine of machines) {
    const free = machineFreeBytes(machine.metrics?.disks);
    if (free === null) continue;
    const wasActive = active.includes(machine.id);
    if (free > (wasActive ? limits.recoverFreeBytes : limits.warnFreeBytes)) continue;
    kept.add(machine.id);
    if (worst === null || free < worst.free) worst = { name: machine.name, free };
  }
  return {
    notice: worst === null ? null : {
      text: `На ${worst.name} мало места: свободно ${fmt.gb(worst.free)}`,
      tone: diskTone(worst.free, limits) === 'danger' ? 'danger' : 'warning'
    },
    active: [...kept]
  };
}
