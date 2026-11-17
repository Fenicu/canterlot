// MCP-сервер forgejo: запуски и задания CI, остальные инструменты — строкой семейства.
import { fmt } from '#lib/format/ru.js';
import { isObject, jsonItems, jsonObjectIn, num, obj, str, type Json, type JsonObject } from '../json';
import { clip, hubError } from '../copy';
import type { Tone } from '../copy';
import type { CardLogic } from '../types';

export type CiStatus = 'success' | 'failure' | 'running' | 'waiting' | 'cancelled' | 'skipped' | 'unknown';

export interface CiRow {
  name: string;
  /** Номер запуска в репозитории, с решёткой впереди. */
  number: string;
  status: CiStatus;
  /** Слово состояния, как его прислал Forgejo, если оно незнакомо. */
  raw: string;
  ref: string;
  /** Сколько шёл запуск или задание. */
  took: string;
  url: string;
}

export interface ForgejoData {
  kind: 'runs' | 'jobs' | 'other';
  op: string;
  repo: string;
  runId: string;
  rows: CiRow[];
  /** Всего по ответу (`total_count`); без него — длина списка. */
  total: number | null;
  error: string;
}

const STATUS: Record<string, CiStatus> = {
  success: 'success',
  succeeded: 'success',
  completed: 'success',
  failure: 'failure',
  failed: 'failure',
  error: 'failure',
  running: 'running',
  in_progress: 'running',
  waiting: 'waiting',
  queued: 'waiting',
  pending: 'waiting',
  blocked: 'waiting',
  cancelled: 'cancelled',
  canceled: 'cancelled',
  skipped: 'skipped'
};

export const CI_WORD: Record<CiStatus, { word: string; tone: Tone }> = {
  success: { word: 'успех', tone: 'success' },
  failure: { word: 'сбой', tone: 'danger' },
  running: { word: 'идёт', tone: 'accent' },
  waiting: { word: 'ждёт', tone: 'warning' },
  cancelled: { word: 'отменён', tone: 'muted' },
  skipped: { word: 'пропущен', tone: 'muted' },
  unknown: { word: '', tone: 'muted' }
};

export function ciStatus(value: Json | undefined): CiStatus {
  return STATUS[str(value).trim().toLowerCase()] ?? 'unknown';
}

/**
 * Длительность запуска. Forgejo отдаёт `duration` как time.Duration Go — наносекунды;
 * без неё считаем от `started` до `stopped`.
 */
export function ciDuration(row: JsonObject): string {
  const duration = num(row.duration);
  if (duration !== null && duration > 0) return fmt.durationCoarse(duration > 1e6 ? duration / 1e6 : duration * 1000);
  if (typeof row.duration === 'string' && row.duration.trim()) return row.duration.trim();
  const started = Date.parse(str(row.started) || str(row.started_at) || str(row.run_started_at));
  const stopped = Date.parse(str(row.stopped) || str(row.completed_at) || str(row.updated_at));
  if (Number.isFinite(started) && Number.isFinite(stopped) && stopped > started) return fmt.durationCoarse(stopped - started);
  return '';
}

function workflowName(row: JsonObject): string {
  const workflow = (str(row.workflow_id) || str(row.workflow) || str(row.path)).replace(/^.*\//, '').replace(/\.ya?ml$/, '');
  return workflow || str(row.name) || clip(str(row.title) || str(row.display_title), 60);
}

function runRow(row: JsonObject): CiRow {
  const number = num(row.index_in_repo) ?? num(row.run_number) ?? num(row.index);
  return {
    name: workflowName(row),
    number: number !== null ? `#${number}` : '',
    status: ciStatus(row.conclusion ?? row.status),
    raw: str(row.conclusion) || str(row.status),
    ref: (str(row.prettyref) || str(row.head_branch) || str(row.ref)).replace(/^refs\/(?:heads|tags)\//, ''),
    took: ciDuration(row),
    url: /^https?:\/\//.test(str(row.html_url)) ? str(row.html_url) : ''
  };
}

function jobRow(row: JsonObject): CiRow {
  return {
    name: str(row.name) || str(row.job_name) || (num(row.id) !== null ? `задание ${num(row.id)}` : 'задание'),
    number: '',
    status: ciStatus(row.conclusion ?? row.status),
    raw: str(row.conclusion) || str(row.status),
    ref: strings(row.runs_on).join(', '),
    took: ciDuration(row),
    url: /^https?:\/\//.test(str(row.html_url)) ? str(row.html_url) : ''
  };
}

function strings(value: Json | undefined): string[] {
  if (typeof value === 'string') return [value];
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}

function kindOf(op: string): ForgejoData['kind'] {
  if (/workflow_runs|action_runs|list_runs/.test(op)) return 'runs';
  if (/jobs/.test(op)) return 'jobs';
  return 'other';
}

export const forgejo: CardLogic<ForgejoData> = {
  label: 'Forgejo',
  parse(call) {
    const op = call.id.slice('forgejo.'.length);
    const input = call.input;
    const repo = [str(input.owner), str(input.repo)].filter(Boolean).join('/');
    const kind = kindOf(op);
    const runId = num(input.run_id) !== null ? String(num(input.run_id)) : str(input.run_id);
    const data: ForgejoData = { kind, op, repo, runId, rows: [], total: null, error: call.state === 'error' ? hubError(call.output) : '' };
    if (kind === 'other' || !call.output || data.error) return data;
    const items = jsonItems(call.output, kind === 'runs' ? ['workflow_runs', 'runs'] : ['jobs']);
    data.rows = items.map(kind === 'runs' ? runRow : jobRow);
    const answer = call.answer ?? jsonObjectIn(call.output);
    data.total = num(answer?.total_count) ?? num(obj(answer ?? {}).total) ?? (items.length > 0 || isObject(answer) ? data.rows.length : null);
    return data;
  },
  summary(_call, data) {
    const where = data.repo ? ` ${data.repo}` : '';
    if (data.kind === 'other') return `Forgejo · ${data.op.replace(/_/g, ' ')}${where}`;
    const head = data.kind === 'runs' ? `CI${where} · запуски` : `CI${where} · задания${data.runId ? ` запуска ${data.runId}` : ''}`;
    if (data.total === null) return head;
    const failed = data.rows.filter((row) => row.status === 'failure').length;
    const running = data.rows.filter((row) => row.status === 'running').length;
    const parts = [`${head}: ${data.total}`];
    if (failed > 0) parts.push(`${failed} ${fmt.plural(failed, ['сбой', 'сбоя', 'сбоев'])}`);
    if (running > 0) parts.push(`${running} ${running === 1 ? 'идёт' : 'идут'}`);
    return parts.join(' · ');
  }
};
