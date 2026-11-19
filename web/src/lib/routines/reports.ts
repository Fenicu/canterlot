// Карточки рутин в ленте родителя: зеркало открытых карточек хаба, тексты отказов и место карточки в ленте.
// Карточка не ввод и не блок ленты: у неё нет порядкового номера, поэтому место ей даёт время создания.
import { SvelteMap } from 'svelte/reactivity';
import type { components } from '#lib/api/generated/openapi.js';
import { stampOf, type Row } from '#lib/timeline/rows.js';

export type RoutineReport = components['schemas']['RoutineReportView'];
export type ReportError = components['schemas']['ReportError'];

const FAILURE_TEXT: Record<string, string> = {
  turn_timeout: 'Копия сессии не ответила за 60 минут',
  parent_busy_timeout: 'Сессия была занята больше двух часов, отчёт не начался',
  copy_failed: 'Копия сессии завершилась с ошибкой',
  copy_lost: 'Копия сессии пропала до конца отчёта',
  empty_report: 'Копия закончила работу без текста ответа',
  no_native_session: 'У сессии ещё нет записи, которую можно прочитать'
};

/** Причина отказа простыми словами; незнакомую причину объясняет строка хаба. */
export function failureText(error: ReportError | null | undefined): string {
  if (!error) return 'Отчёт не собран';
  return FAILURE_TEXT[error.reason] ?? (error.message.trim() || 'Отчёт не собран');
}

function createdAt(report: RoutineReport): number {
  return Date.parse(report.created_at) || 0;
}

/** Открытые карточки разговора. Закрытая карточка не воскресает от запоздавшего события. */
export class RoutineReportsModel {
  readonly #cards = new SvelteMap<string, RoutineReport>();
  readonly #closed = new Set<string>();

  get list(): RoutineReport[] {
    return [...this.#cards.values()].sort((a, b) => createdAt(a) - createdAt(b) || a.id.localeCompare(b.id));
  }

  get(id: string): RoutineReport | undefined {
    return this.#cards.get(id);
  }

  apply(report: RoutineReport): void {
    if (this.#closed.has(report.id)) return;
    this.#cards.set(report.id, report);
  }

  dismiss(id: string): void {
    this.#closed.add(id);
    this.#cards.delete(id);
  }

  /** Снимок заменяет набор целиком; то, что владелец закрыл после него, остаётся закрытым. */
  ingestSnapshot(reports: RoutineReport[]): void {
    const next = new Map(reports.filter((item) => !this.#closed.has(item.id)).map((item) => [item.id, item]));
    for (const id of [...this.#cards.keys()]) if (!next.has(id)) this.#cards.delete(id);
    for (const [id, item] of next) this.#cards.set(id, item);
  }

  clear(): void {
    this.#cards.clear();
    this.#closed.clear();
  }
}

/**
 * Строки ленты с карточками на местах по времени: карточка встаёт перед первой строкой, которая
 * новее её, но не между подписью дня и первой строкой дня. Окно, оторванное от хвоста, не показывает
 * карточки новее своих строк: между ними дыра.
 */
export function placeReports(
  rows: Row[],
  reports: RoutineReport[],
  opts: { hasOlder: boolean; tailDetached: boolean }
): Row[] {
  if (reports.length === 0) return rows;
  const result = rows.slice();
  const ordered = [...reports].sort((a, b) => createdAt(a) - createdAt(b) || a.id.localeCompare(b.id));
  const stamps = (list: Row[]): number[] => list.map((row) => (row.kind === 'report' ? createdAtOf(row) : (row.entries[0] ? stampOf(row.entries[0]) : 0)));
  for (const report of ordered) {
    const when = createdAt(report);
    const stamp = stamps(result);
    let index = result.length;
    for (let i = 0; i < result.length; i++) {
      if (stamp[i]! > when) {
        index = i;
        break;
      }
    }
    if (index === result.length && opts.tailDetached) continue;
    if (index > 0 && result[index - 1]!.kind === 'day') index -= 1;
    result.splice(index, 0, { key: `routine:${report.id}`, kind: 'report', entries: [], report });
  }
  return result;
}

function createdAtOf(row: Row): number {
  return row.report ? createdAt(row.report) : 0;
}
