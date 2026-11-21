// Задачи на месте (`job_run`): слова состояний, отказов и причин конца, ссылка на лист задачи.
// Одно место для карточек ленты, листа задачи и раздела «Задачи» на экране машины.
import type { components } from '#lib/api/generated/openapi.js';
import { fmt } from '#lib/format/ru.js';

export type JobView = components['schemas']['JobView'];

export type JobTone = 'accent' | 'blue' | 'success' | 'warning' | 'danger' | 'muted';

export interface JobWord {
  word: string;
  tone: JobTone;
  /** Задача идёт: точка горит акцентом. */
  live?: boolean;
}

/** То, что нужно словам и спискам от задачи; одинаково для строки хаба и для разобранного ответа. */
export interface JobFacts {
  id: string;
  state: string;
  machineId: string;
  machineName: string;
  title: string | null;
  shell: string;
  workdir: string;
  exitCode: number | null;
  durationMs: number | null;
  cancelRequested: boolean;
  machineOffline: boolean;
  endReason: string | null;
  createdAt: string;
}

/** Строка хаба (`JobView`) в виде, который читают слова и списки. */
export function jobFacts(view: JobView): JobFacts {
  return {
    id: view.id,
    state: view.state,
    // Пока задача ждёт раннер, машины нет: пустая строка не совпадает ни с одной машиной.
    machineId: view.machine_id ?? '',
    machineName: view.machine_name ?? '',
    title: view.title ?? null,
    shell: view.shell,
    workdir: view.workdir ?? '',
    exitCode: view.exit_code ?? null,
    durationMs: view.duration_ms ?? null,
    cancelRequested: view.cancel_requested,
    machineOffline: view.machine_offline,
    endReason: view.end_reason ?? null,
    createdAt: view.created_at
  };
}

const TERMINAL = new Set(['succeeded', 'failed', 'timed_out', 'canceled', 'lost']);
// `waiting_runner`, `staging` и `collecting` — ещё не конец: итог придёт после сбора результатов.

export function isTerminal(state: string): boolean {
  return TERMINAL.has(state);
}

/** Состояние задачи одним словом. */
export function jobWord(job: Pick<JobFacts, 'state' | 'exitCode' | 'cancelRequested'>): JobWord {
  switch (job.state) {
    case 'waiting_runner':
      return { word: 'ждёт раннер', tone: 'warning' };
    case 'staging':
      return { word: 'готовит исходники', tone: 'accent', live: true };
    case 'collecting':
      return { word: 'собирает результаты', tone: 'accent', live: true };
    case 'queued':
      return { word: 'в очереди', tone: 'muted' };
    case 'dispatched':
      return { word: 'передана машине', tone: 'muted' };
    case 'waiting_lease':
      return { word: 'ждёт очереди сборок', tone: 'warning' };
    case 'running':
      return job.cancelRequested ? { word: 'отменяется', tone: 'warning', live: true } : { word: 'идёт', tone: 'accent', live: true };
    case 'succeeded':
      return { word: 'готово', tone: 'success' };
    case 'failed':
      return job.exitCode !== null && job.exitCode !== 0 ? { word: `код ${job.exitCode}`, tone: 'danger' } : { word: 'ошибка', tone: 'danger' };
    case 'timed_out':
      return { word: 'время вышло', tone: 'danger' };
    case 'canceled':
      return { word: 'отменена', tone: 'muted' };
    case 'lost':
      return { word: 'потеряна', tone: 'danger' };
    default:
      return { word: job.state, tone: 'muted' };
  }
}

/** Почему состояние не свежее: потерянная задача и незаконченная на машине без связи. */
export function offlineNote(job: Pick<JobFacts, 'state' | 'machineOffline'>): string {
  if (job.state === 'lost') return 'связь с машиной потеряна';
  return job.machineOffline && !isTerminal(job.state) ? 'машина не на связи' : '';
}

export function durationWord(ms: number | null | undefined): string {
  return typeof ms === 'number' && Number.isFinite(ms) ? fmt.duration(ms) : '';
}

/** Название задачи: своё или первая строка команды, не длиннее восьмидесяти знаков. */
export function jobLabel(title: string | null | undefined, shell: string | null | undefined, max = 80): string {
  const own = (title ?? '').replace(/\s+/g, ' ').trim();
  const text = own || ((shell ?? '').split('\n').find((line) => line.trim()) ?? '').trim();
  return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text;
}

/** Лист задачи открывается на экране машин: так же, как запись памяти — на экране «Память». */
export function jobHref(id: string): string {
  return `/machines?job=${encodeURIComponent(id)}`;
}

/** Отказ словами: по коду отказа хаба или причине конца задачи. */
export function refusalText(code: string): string {
  switch (code) {
    case 'mode_too_narrow':
      return 'Отказ: режим не позволяет';
    case 'domain_mismatch':
      return 'Отказ: другой домен памяти';
    case 'remote_work_disabled':
      return 'Машина не принимает работу с других машин';
    case 'capability_missing':
      return 'Воркер машины устарел';
    case 'workdir_not_allowed':
    case 'workdir_refused':
      return 'Каталог не подходит';
    case 'source_required':
      return 'Для автоматического размещения нужны исходники';
    case 'no_runner_can_satisfy':
      return 'Ни один раннер не подходит по меткам';
    case '':
      return 'Отказ: вызов не удался';
    default:
      return `Отказ: ${code}`;
  }
}

/** Причина конца задачи, как её хранит хаб. */
export function reasonText(reason: string | null | undefined): string {
  switch (reason ?? '') {
    case '':
      return '';
    case 'canceled_by_owner':
      return 'отменена владельцем';
    case 'canceled_by_caller':
      return 'отменена агентом';
    case 'launch_interrupted':
      return 'запуск прерван сбоем машины';
    default:
      return refusalText(reason!);
  }
}

const RECENT = 10;

/** Раздел «Задачи» машины: все незаконченные и десять последних законченных, новые первыми. */
export function machineJobs<T extends Pick<JobFacts, 'id' | 'state' | 'machineId' | 'createdAt'>>(jobs: Iterable<T>, machineId: string): { running: T[]; recent: T[] } {
  const own = [...jobs].filter((job) => job.machineId === machineId).sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt) || b.id.localeCompare(a.id));
  return { running: own.filter((job) => !isTerminal(job.state)), recent: own.filter((job) => isTerminal(job.state)).slice(0, RECENT) };
}
