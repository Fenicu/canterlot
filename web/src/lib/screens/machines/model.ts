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

/** Предупреждения сторожа машины показаны своими блоками, а не общей строкой. */
const WATCH_WARNINGS = new Set(['memory_low', 'processes_leaked']);

export function generalWarnings(values: string[]): string[] {
  return machineWarnings(values.filter(value => !WATCH_WARNINGS.has(value)));
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

type HotspotView = components['schemas']['HotspotView'];
type LeakedView = components['schemas']['LeakedView'];

/** Размер в десятичных гигабайтах; меньше десяти — с десятыми: «1,2 ГБ», «77 ГБ». */
export function sizeGb(bytes: number): string {
  const value = bytes / GB;
  if (value >= 9.95) return fmt.gb(bytes);
  return `${value.toFixed(1).replace('.', ',')} ГБ`;
}

/** Короткая давность для плотных строк: «12 мин», «5 ч», «3 д». */
export function shortAge(ms: number): string {
  const minutes = Math.max(0, Math.floor(ms / 60_000));
  if (minutes < 1) return 'меньше минуты';
  if (minutes < 60) return `${minutes} мин`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} ч`;
  return `${Math.floor(hours / 24)} д`;
}

/** Хвост пути из двух последних каталогов: «…/01a1b8e2/target». */
export function shortPath(path: string): string {
  const parts = path.split('/').filter(Boolean);
  return parts.length <= 2 ? path : `…/${parts.slice(-2).join('/')}`;
}

export interface HotspotRow {
  key: string;
  name: string;
  path: string | null;
  size: string;
  meta: string;
}

/** Инкрементальный кэш стоит называть, когда он весит хотя бы гигабайт и десятую часть каталога. */
function incrementalShown(spot: HotspotView): boolean {
  return spot.incremental_bytes >= GB && spot.incremental_bytes * 10 >= spot.bytes;
}

export function hotspotRows(hotspots: HotspotView[], now: number, limit = 5): HotspotRow[] {
  return [...hotspots]
    .sort((a, b) => b.bytes - a.bytes)
    .slice(0, limit)
    .map(spot => {
      const newest = spot.newest_at ? Date.parse(spot.newest_at) : Number.NaN;
      const meta = [
        incrementalShown(spot) ? `из них incremental ${sizeGb(spot.incremental_bytes)}` : '',
        Number.isNaN(newest) ? '' : `запись ${shortAge(now - newest)} назад`
      ].filter(Boolean).join(' · ');
      return {
        key: spot.path,
        name: spot.title ? `Рабочая копия «${spot.title}»` : spot.path,
        path: spot.title ? shortPath(spot.path) : null,
        size: sizeGb(spot.bytes),
        meta
      };
    });
}

export interface LeakedRow {
  key: string;
  name: string;
  processes: { key: string; text: string }[];
}

export interface LeakedSummary {
  title: string;
  meta: string;
  rows: LeakedRow[];
}

function mib(bytes: number): string {
  return fmt.bytes(Math.round(bytes / 1024 ** 2) * 1024 ** 2);
}

/** Процессы без сессии, сгруппированные по запуску: старшие запуски первыми. */
export function leakedSummary(leaked: LeakedView[]): LeakedSummary | null {
  if (leaked.length === 0) return null;
  const counts = new Map<string, number>();
  for (const item of leaked) counts.set(item.comm, (counts.get(item.comm) ?? 0) + 1);
  const byComm = [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([comm, count]) => `${comm} ${count}`).join(', ');
  const rss = leaked.reduce((sum, item) => sum + item.rss_bytes, 0);
  const runs = new Map<string, LeakedView[]>();
  for (const item of [...leaked].sort((a, b) => b.age_s - a.age_s)) runs.set(item.execution_id, [...(runs.get(item.execution_id) ?? []), item]);
  const n = leaked.length;
  return {
    title: n === 1 ? '1 процесс пережил свою сессию' : `${n} ${fmt.plural(n, ['процесс', 'процесса', 'процессов'])} пережили свои сессии`,
    meta: `${byComm} · занимают ${mib(rss)}`,
    rows: [...runs].map(([execution, items]) => ({
      key: execution,
      // У UUIDv7 начало — время запуска и у соседних запусков совпадает; различает хвост.
      name: items[0]!.title ?? `Запуск ${execution.slice(-8)}`,
      processes: items.map(item => ({ key: String(item.pid), text: `${item.comm} · pid ${item.pid} · ${shortAge(item.age_s * 1000)} · ${mib(item.rss_bytes)}` }))
    }))
  };
}

export function unleasedText(count: number): string | null {
  return count > 0 ? `Без очереди: ${count} ${fmt.plural(count, ['сборка', 'сборки', 'сборок'])}` : null;
}

export function memoryLowText(availableBytes: number | undefined): string {
  return availableBytes === undefined ? 'Мало памяти' : `Мало памяти: ${fmt.bytes(availableBytes)} свободно`;
}

