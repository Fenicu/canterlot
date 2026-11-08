import type { components } from '#lib/api/generated/openapi.js';
import { fmt } from '#lib/format/ru.js';

export type LeaseState = components['schemas']['LeaseStateView'];
export type LeaseEntry = components['schemas']['LeaseEntryView'];

/** Место исполнения в очереди тяжёлых задач из свидетельства активности `working`. */
export interface LeaseWait {
  kind: string;
  position: number;
}

export function leaseKindLabel(kind: string): string {
  return kind === 'gate' ? 'гейт' : 'сборка';
}

/** «Ждёт очереди на сборку · 2-й»: строка вместо «Выполняет Bash», пока процесс стоит в очереди. */
export function leaseWaitText(wait: LeaseWait): string {
  const goal = wait.kind === 'gate' ? 'на гейт' : 'на сборку';
  return `Ждёт очереди ${goal} · ${Math.max(1, Math.floor(wait.position))}-й`;
}

/** Ожидание очереди из активности сессии; у старого хаба и вне работы его нет. */
export function leaseWaitOf(activity: unknown): LeaseWait | null {
  const value = activity as { state?: unknown; evidence?: { lease_wait?: unknown } } | null | undefined;
  if (value?.state !== 'working') return null;
  const wait = value.evidence?.lease_wait as { kind?: unknown; position?: unknown } | null | undefined;
  if (!wait || typeof wait.kind !== 'string' || typeof wait.position !== 'number' || !Number.isFinite(wait.position)) return null;
  return { kind: wait.kind, position: wait.position };
}

export interface LeaseRow {
  key: string;
  name: string;
  kind: string;
  /** Сколько держит (у держателей) или место в очереди (у ожидающих). */
  detail: string;
  conversationId: string | null;
}

function holderName(entry: LeaseEntry, titleOf: (id: string) => string | undefined): string {
  if (entry.holder === 'owner') return 'вы';
  const title = entry.conversation_id ? titleOf(entry.conversation_id)?.trim() : undefined;
  return title || entry.label.trim() || entry.dir.trim() || 'сессия';
}

/** Держатели и ожидающие строками экрана машины; пустая очередь — пустые списки. */
export function leaseRows(
  lease: LeaseState | null | undefined,
  titleOf: (id: string) => string | undefined,
  now: number
): { holders: LeaseRow[]; waiters: LeaseRow[] } {
  if (!lease) return { holders: [], waiters: [] };
  const row = (entry: LeaseEntry, detail: string): LeaseRow => ({
    key: entry.lease_id,
    name: holderName(entry, titleOf),
    kind: leaseKindLabel(entry.kind),
    detail,
    conversationId: entry.holder === 'execution' ? entry.conversation_id ?? null : null
  });
  const held = (entry: LeaseEntry) => {
    const since = Date.parse(entry.since ?? '');
    return Number.isFinite(since) ? fmt.durationCoarse(Math.max(0, now - since)) : '';
  };
  return {
    holders: lease.holders.map((entry) => row(entry, held(entry))),
    waiters: [...lease.waiters]
      .sort((a, b) => (a.position ?? 0) - (b.position ?? 0))
      .map((entry) => row(entry, `${entry.position ?? 1}-й`))
  };
}
