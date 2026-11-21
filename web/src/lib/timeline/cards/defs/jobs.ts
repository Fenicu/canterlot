// Команды на месте (`job_run`): запуск, состояние, журнал, отмена и список. Ответы хаба: `session_tools/inplace_jobs.rs`.
import { fmt } from '#lib/format/ru.js';
import { isTerminal, jobHref, jobLabel, jobWord, refusalText } from '#lib/jobs/model.js';
import { bool, list, num, str, isObject, type JsonObject } from '../json';
import { say } from '../copy';
import type { CardCall, CardLogic, StateWord } from '../types';

/** Код отказа хаба: первое слово ответа вида `код: пояснение`; пусто, если вызов не сорван. */
export function refusalCode(call: CardCall): string | null {
  if (call.state !== 'error') return null;
  const first = call.output.split('\n').find((part) => part.trim()) ?? '';
  return /^([a-z_]+):/.exec(first.trim())?.[1] ?? '';
}

const running = (call: CardCall): boolean => call.state === 'running';

function shortId(id: string): string {
  return id.slice(0, 8);
}

/* ------------------------------------------------------------ run, status, cancel */

export interface JobCallData {
  tool: 'run' | 'status' | 'cancel';
  jobId: string;
  /** Состояние из ответа; пусто, пока вызов ещё ждёт ответа. */
  state: string;
  machine: string;
  title: string;
  shell: string;
  workdir: string;
  exitCode: number | null;
  durationMs: number | null;
  /** Хвост вывода: он есть только в итоговом ответе. */
  outputTail: string;
  outputBytes: number | null;
  truncated: boolean;
  /** Причина конца, как её хранит хаб. */
  reason: string;
  cancelRequested: boolean;
  offline: boolean;
  /** Воркер заговорил о потерянной задаче позже. */
  lateReport: boolean;
  /** Код отказа; `null` — вызов не отказан. */
  refusal: string | null;
  /** Размещение выбирает хаб (`machine: "auto"`): машину задачи называет ответ, а не вызов. */
  auto: boolean;
  /** Исходники из вызова: репозиторий и ревизия. */
  source: { repo: string; rev: string } | null;
  /** Каталог на несущей, куда собраны результаты; только в итоговом ответе. */
  outputsDir: string;
  outputsNote: string;
}

function sourceOf(value: unknown): JobCallData['source'] {
  if (!isObject(value as never)) return null;
  const source = value as JsonObject;
  const repo = str(source.repo);
  const rev = str(source.rev);
  return repo && rev ? { repo, rev } : null;
}

function jobParse(tool: JobCallData['tool']): CardLogic<JobCallData>['parse'] {
  return (call) => {
    const refusal = refusalCode(call);
    const answer = call.answer;
    const jobId = str(answer?.job_id) || str(call.input.job_id);
    const waiting = running(call) && (tool === 'run' ? call.hasInput : jobId !== '');
    const answered = answer !== null && str(answer.job_id) !== '' && str(answer.state) !== '';
    if (refusal === null && !answered && !waiting) return null;
    if (tool !== 'run' && refusal === null && !jobId) return null;
    const a = answered ? answer : null;
    const auto = str(call.input.machine) === 'auto';
    return {
      tool,
      jobId,
      state: str(a?.state),
      // `auto` — не имя машины: пока хаб не выбрал раннер, места в строке нет.
      machine: str(a?.machine) || (auto ? '' : str(call.input.machine)),
      title: str(call.input.title).trim(),
      shell: str(call.input.shell),
      workdir: str(call.input.workdir),
      exitCode: num(a?.exit_code),
      durationMs: num(a?.duration_ms),
      outputTail: str(a?.output_tail),
      outputBytes: num(a?.output_bytes),
      truncated: bool(a?.truncated) === true,
      reason: str(a?.reason),
      cancelRequested: bool(a?.cancel_requested) === true,
      offline: bool(a?.machine_offline) === true,
      lateReport: isObject(a?.late_report as never),
      refusal,
      auto,
      source: sourceOf(call.input.source),
      outputsDir: str(a?.outputs_dir),
      outputsNote: str(a?.outputs_note)
    };
  };
}

/** Слово состояния по ответу; у отказанного и ещё идущего вызова слово даёт сам вызов. */
export function jobCallWord(data: JobCallData): StateWord | null {
  if (data.refusal !== null || !data.state) return null;
  return jobWord({ state: data.state, exitCode: data.exitCode, cancelRequested: data.cancelRequested });
}

function onMachine(machine: string): string {
  return machine ? ` на ${machine}` : '';
}

export const jobRun: CardLogic<JobCallData> = {
  stays: true,
  label: 'Команда',
  parse: jobParse('run'),
  summary(call, data) {
    if (data.refusal !== null) return refusalText(data.refusal);
    const label = jobLabel(data.title, data.shell);
    const place = `${onMachine(data.machine)}${label ? `: ${label}` : ''}`;
    if (running(call)) return say(call.voice, `Запускает команду${place}`, `Запуск команды${place}`);
    return say(call.voice, `Запустила команду${place}`, `Команда${place}`);
  },
  link(_call, data) {
    const label = jobLabel(data.title, data.shell);
    return data.refusal === null && data.jobId && label ? { text: label, href: jobHref(data.jobId) } : null;
  }
};

function byId(call: CardCall, data: JobCallData, she: string, neutral: string): string {
  if (data.refusal !== null) return refusalText(data.refusal);
  return `${say(call.voice, she, neutral)} ${shortId(data.jobId)}${onMachine(data.machine)}`;
}

const idLink: CardLogic<JobCallData>['link'] = (_call, data) => (data.refusal === null && data.jobId ? { text: shortId(data.jobId), href: jobHref(data.jobId) } : null);

export const jobStatus: CardLogic<JobCallData> = {
  label: 'Команда',
  parse: jobParse('status'),
  summary: (call, data) => byId(call, data, 'Проверила задачу', 'Состояние задачи'),
  link: idLink
};

export const jobCancel: CardLogic<JobCallData> = {
  stays: true,
  label: 'Команда',
  parse: jobParse('cancel'),
  summary(call, data) {
    if (data.cancelRequested && !isTerminal(data.state)) return byId(call, data, 'Попросила отменить задачу', 'Запрос отмены задачи');
    return byId(call, data, 'Отменила задачу', 'Отмена задачи');
  },
  link: idLink
};

/* ------------------------------------------------------------------ журнал */

export interface JobLogData {
  jobId: string;
  state: string;
  /** `tail` — хвост хаба, `log` — полный журнал после конца. */
  source: string;
  offset: number | null;
  nextOffset: number | null;
  totalBytes: number | null;
  eof: boolean;
  text: string;
  /** Полный журнал убран по сроку хранения: остался хвост. */
  expired: boolean;
  refusal: string | null;
}

export const jobLog: CardLogic<JobLogData> = {
  label: 'Команда',
  parse(call) {
    const refusal = refusalCode(call);
    const answer = call.answer;
    const jobId = str(answer?.job_id) || str(call.input.job_id);
    const answered = answer !== null && str(answer.job_id) !== '' && typeof answer.text === 'string';
    if (refusal === null && !answered && !(running(call) && jobId)) return null;
    const a = answered ? answer : null;
    return {
      jobId,
      state: str(a?.state),
      source: str(a?.source),
      offset: num(a?.offset),
      nextOffset: num(a?.next_offset),
      totalBytes: num(a?.total_bytes),
      eof: bool(a?.eof) === true,
      text: str(a?.text),
      expired: str(a?.note) === 'full log expired',
      refusal
    };
  },
  summary(call, data) {
    if (data.refusal !== null) return refusalText(data.refusal);
    const id = shortId(data.jobId);
    return running(call) ? `Чтение журнала задачи ${id}` : `${say(call.voice, 'Читала журнал задачи', 'Журнал задачи')} ${id}`;
  },
  link: (_call, data) => (data.refusal === null && data.jobId ? { text: shortId(data.jobId), href: jobHref(data.jobId) } : null)
};

/** Слово справа у журнала: состояние задачи, если хаб его назвал. */
export function logWord(data: JobLogData): StateWord | null {
  return data.refusal === null && data.state ? jobWord({ state: data.state, exitCode: null, cancelRequested: false }) : null;
}

/* ------------------------------------------------------------------ список */

export interface JobRow {
  jobId: string;
  state: string;
  machine: string;
  title: string;
  /** Начало команды: хаб отдаёт не больше ста двадцати знаков. */
  shell: string;
  exitCode: number | null;
  durationMs: number | null;
  offline: boolean;
  createdAt: string;
}

export interface JobListData {
  rows: JobRow[];
  refusal: string | null;
}

function rowOf(value: unknown): JobRow | null {
  if (!isObject(value as never)) return null;
  const item = value as JsonObject;
  const jobId = str(item.job_id);
  if (!jobId) return null;
  return {
    jobId,
    state: str(item.state),
    machine: str(item.machine),
    title: str(item.title),
    shell: str(item.shell),
    exitCode: num(item.exit_code),
    durationMs: num(item.duration_ms),
    offline: bool(item.machine_offline) === true,
    createdAt: str(item.created_at)
  };
}

/* ------------------------------------------------------------------ раннеры */

export interface RunnerRow {
  name: string;
  /** `online`, `offline` или `unknown`: слова хаба. */
  availability: string;
  /** Возраст свежей проверки несущей; у `online` и `unknown` его нет. */
  ageSeconds: number | null;
  slotsUsed: number;
  slotsMax: number;
  /** Владелец поставил раннер на паузу. */
  paused: boolean;
  /** Эта строка — сама несущая, а не раннер. */
  local: boolean;
}

export interface RunnersListData {
  rows: RunnerRow[];
  /** Задачи несущей, которым ещё не нашёлся раннер. */
  waiting: number;
  refusal: string | null;
}

function runnerRow(value: unknown): RunnerRow | null {
  if (!isObject(value as never)) return null;
  const item = value as JsonObject;
  const name = str(item.name);
  if (!name) return null;
  return {
    name,
    availability: str(item.availability) || 'unknown',
    ageSeconds: num(item.observation_age_seconds),
    slotsUsed: num(item.slots_used) ?? 0,
    slotsMax: num(item.slots_max) ?? 0,
    paused: bool(item.paused) === true,
    local: str(item.role) === 'local'
  };
}

export const runnersList: CardLogic<RunnersListData> = {
  label: 'Раннеры',
  parse(call) {
    const refusal = refusalCode(call);
    if (refusal !== null) return { rows: [], waiting: 0, refusal };
    if (!Array.isArray(call.answer?.runners) && !running(call)) return null;
    return { rows: list(call.answer?.runners).map(runnerRow).filter((row): row is RunnerRow => row !== null), waiting: num(call.answer?.waiting_runner_jobs) ?? 0, refusal: null };
  },
  summary(call, data) {
    if (data.refusal !== null) return refusalText(data.refusal);
    const head = running(call) ? 'Поиск раннеров' : say(call.voice, 'Смотрела раннеры', 'Раннеры');
    if (running(call)) return head;
    if (data.rows.length === 0) return `${head}: нет`;
    const online = data.rows.filter((row) => row.availability === 'online').length;
    const parts = [`${online} на связи`, data.waiting > 0 ? `ждут ${data.waiting}` : ''].filter(Boolean);
    return `${head}: ${data.rows.length} · ${parts.join(' · ')}`;
  }
};

const QUEUED = new Set(['queued', 'dispatched', 'waiting_lease']);

export const jobsList: CardLogic<JobListData> = {
  label: 'Команда',
  parse(call) {
    const refusal = refusalCode(call);
    if (refusal !== null) return { rows: [], refusal };
    if (!Array.isArray(call.answer?.jobs) && !running(call)) return null;
    return { rows: list(call.answer?.jobs).map(rowOf).filter((row): row is JobRow => row !== null), refusal: null };
  },
  summary(call, data) {
    if (data.refusal !== null) return refusalText(data.refusal);
    const head = running(call) ? 'Поиск задач' : say(call.voice, 'Смотрела задачи', 'Задачи');
    if (running(call)) return head;
    if (data.rows.length === 0) return `${head}: нет`;
    const going = data.rows.filter((row) => row.state === 'running').length;
    const queued = data.rows.filter((row) => QUEUED.has(row.state)).length;
    const parts = [going ? `${going} ${fmt.plural(going, ['идёт', 'идут', 'идут'])}` : '', queued ? `${queued} в очереди` : ''].filter(Boolean);
    return `${head}: ${data.rows.length}${parts.length ? ` · ${parts.join(', ')}` : ''}`;
  }
};
