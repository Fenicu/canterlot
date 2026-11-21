// Задачи на месте (`job_run`) в фейковом хабе: строки, события `job_changed` и HTTP владельца.
import type { Page } from '@playwright/test';
import type { components } from '../../src/lib/api/generated/openapi.js';
import type { FakeHubApi } from './api';
import type { FakeHub } from './hub';

export type FakeJob = components['schemas']['JobView'];

const TERMINAL = new Set(['succeeded', 'failed', 'timed_out', 'canceled', 'lost']);

/** Задача на машине `machine`; без правок — поставленная в очередь команда. Лежит в `api.jobs`. */
export function seedJob(api: FakeHubApi, machine: { id: string; name: string }, patch: Partial<FakeJob> = {}, log = ''): FakeJob {
  const job: FakeJob = {
    id: api.id(),
    state: 'queued',
    machine_id: machine.id,
    machine_name: machine.name,
    conversation_id: null,
    conversation_title: null,
    title: null,
    workdir: '/h/u/proj',
    shell: 'npm run build',
    lease: 'none',
    timeout_minutes: 30,
    exit_code: null,
    duration_ms: null,
    output_bytes: log.length,
    truncated: false,
    end_reason: null,
    cancel_requested: false,
    has_log: false,
    machine_offline: false,
    late_report: null,
    labels: [],
    expected_minutes: 30,
    source: null,
    outputs_dir: null,
    outputs_note: null,
    created_at: new Date().toISOString(),
    started_at: null,
    finished_at: null,
    ...patch
  };
  api.jobs.set(job.id, job);
  if (log) api.jobLogs.set(job.id, log);
  return job;
}

/** Хаб сообщает о смене задачи: правка строки, вывод и событие `job_changed` в области машин. */
export function changeJob(hub: FakeHub, api: FakeHubApi, id: string, patch: Partial<FakeJob> = {}, log?: string): FakeJob {
  const old = api.jobs.get(id);
  if (!old) throw new Error(`no job ${id}`);
  if (log !== undefined) api.jobLogs.set(id, log);
  const next: FakeJob = { ...old, ...patch, output_bytes: log !== undefined ? log.length : (patch.output_bytes ?? old.output_bytes) };
  if (TERMINAL.has(next.state) && !next.finished_at) next.finished_at = new Date().toISOString();
  api.jobs.set(id, next);
  hub.append('machines', { kind: 'job_changed', payload: { job: next } });
  hub.flush();
  return next;
}

function errorBody(code: string, message: string): unknown {
  return { error: { code, message, scope: 'delivery', retry: 'explicit_new_operation', correlation_id: '00000000-0000-4000-8000-0000000000a9' } };
}

function headers(): Record<string, string> {
  return { 'X-Server-Time-Ms': String(Date.now()) };
}

const encoder = new TextEncoder();

/** Список, карточка, журнал и отмена: так их отдаёт хаб владельцу. */
export async function installJobRoutes(page: Page, hub: FakeHub, api: FakeHubApi): Promise<void> {
  await page.route(/\/api\/v1\/jobs(?:\?.*)?$/, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const query = new URL(route.request().url()).searchParams;
    const limit = Number(query.get('limit') ?? 50);
    const items = [...api.jobs.values()]
      .filter((job) => (!query.get('machine') || job.machine_id === query.get('machine')) && (!query.get('state') || job.state === query.get('state')) && (!query.get('conversation') || job.conversation_id === query.get('conversation')))
      .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at) || b.id.localeCompare(a.id))
      .slice(0, limit);
    await route.fulfill({ headers: headers(), json: { items } });
  });

  await page.route(/\/api\/v1\/jobs\/[^/?]+(?:\/log|\/cancel)?(?:\?.*)?$/, async (route) => {
    const url = new URL(route.request().url());
    const [, , , , id = '', action = ''] = url.pathname.split('/');
    const job = api.jobs.get(id);
    if (!job) return route.fulfill({ status: 404, headers: headers(), json: errorBody('not_found', 'Задача не найдена') });
    const method = route.request().method();
    const full = api.jobLogs.get(id) ?? '';
    const bytes = encoder.encode(full);
    if (action === '' && method === 'GET') {
      return route.fulfill({ headers: headers(), json: { job, output_tail: new TextDecoder().decode(bytes.slice(-65_536)) } });
    }
    if (action === 'log' && method === 'GET') {
      const offsetText = url.searchParams.get('offset');
      const limit = Number(url.searchParams.get('limit') ?? 16_384);
      api.jobLogCalls.push({ id, offset: offsetText, limit: url.searchParams.get('limit') });
      const expired = api.jobLogsExpired.has(id);
      // Полный журнал есть только у законченной задачи, пока его не убрали по сроку; иначе — хвост хаба.
      const fromLog = TERMINAL.has(job.state) && !expired && offsetText !== null;
      const start = fromLog ? Math.min(Number(offsetText), bytes.length) : Math.max(0, bytes.length - limit);
      const end = Math.min(bytes.length, start + limit);
      return route.fulfill({
        headers: headers(),
        json: { source: fromLog ? 'log' : 'tail', offset: start, next_offset: end, total_bytes: bytes.length, eof: end >= bytes.length, text: new TextDecoder().decode(bytes.slice(start, end)), note: expired ? 'full log expired' : null }
      });
    }
    if (action === 'cancel' && method === 'POST') {
      const key = route.request().headers()['idempotency-key'];
      if (!key) return route.fulfill({ status: 400, headers: headers(), json: errorBody('invalid_request', 'Нужен Idempotency-Key') });
      api.jobCancelCalls.push({ id, key });
      if (TERMINAL.has(job.state) && job.state !== 'lost') return route.fulfill({ headers: headers(), json: job });
      // Очередь закрывается сразу; идущую задачу закроет воркер, когда подтвердит, что процессы исчезли.
      const queued = ['queued', 'dispatched', 'waiting_lease'].includes(job.state);
      const next = changeJob(hub, api, id, queued ? { state: 'canceled', end_reason: 'canceled_by_owner', cancel_requested: true } : { cancel_requested: true });
      return route.fulfill({ headers: headers(), json: next });
    }
    return route.fallback();
  });
}
