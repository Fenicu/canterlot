// Лента оркестратора с задачами на раннерах: несущая, раннер под ней и вызовы `job_run` с `machine: "auto"`.
import type { FakeHubApi } from '../fake-hub/api';
import { seedJob, type FakeJob } from '../fake-hub/jobs';
import { seedConversation, seedMachine } from '../fake-hub/seed';
import { Feed, hubText } from './feed-cards';

const MODEL = 'claude-opus-5-5';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const MINUTE = 60_000;

export const OUTPUTS_DIR = '/srv/runner-outputs/gate-1';
export const SOURCE = { repo: 'project', rev: '1a2b3c4' };

export interface RunnerJobsSeed {
  id: string;
  carrier: { id: string; name: string };
  runner: { id: string; name: string };
  /** Ждёт раннер: дальше его ведут события хаба по всему пути до конца. */
  flow: FakeJob;
  /** Подвоз кода сорван. */
  stagingFailed: FakeJob;
  /** Итог получен, результаты не собраны. */
  notCollected: FakeJob;
  /** Итог получен, результаты собраны. */
  collected: FakeJob;
  blocks: Record<string, string>;
}

const AUTO = { machine: 'auto', shell: 'scripts/gate.sh', source: SOURCE, labels: ['build-host'], outputs: ['result'], wait_seconds: 20 };

export function seedRunnerJobCards(api: FakeHubApi, opts: { now?: number } = {}): RunnerJobsSeed {
  const now = opts.now ?? Date.now();
  const carrier = seedMachine(api, { name: 'carrier-a', is_carrier: true, accepts_remote_work: true });
  const runner = seedMachine(api, { name: 'runner-a', role: 'runner', parent_machine_id: carrier.id, rank: 20, labels: ['build-host'], availability: 'online', observed_at: null, accepts_remote_work: true, harnesses: [] });
  const id = seedConversation(api, { title: 'Кантерлот', model: MODEL, harness: CLAUDE });
  const at = (ms: number) => new Date(now + ms).toISOString();
  const conversation = { conversation_id: id, conversation_title: 'Кантерлот' };
  const common = { ...conversation, shell: 'scripts/gate.sh', source: SOURCE, labels: ['build-host'], workdir: null as string | null };

  const flow = seedJob(api, null, { ...common, state: 'waiting_runner', title: 'Полная проверка', created_at: at(-3 * MINUTE) });
  const stagingFailed = seedJob(api, runner, { ...common, state: 'failed', title: 'Проверка без кода', end_reason: 'staging_failed: git push failed', created_at: at(-12 * MINUTE), finished_at: at(-11 * MINUTE) });
  const notCollected = seedJob(api, runner, { ...common, state: 'succeeded', title: 'Проверка без результатов', workdir: '/srv/runner/jobs/work/nc', exit_code: 0, duration_ms: 61_000, outputs_note: 'outputs not collected: collection timed out', created_at: at(-20 * MINUTE), started_at: at(-19 * MINUTE), finished_at: at(-18 * MINUTE) }, 'all steps passed\n');
  const collected = seedJob(api, runner, { ...common, state: 'succeeded', title: 'Проверка с результатами', workdir: '/srv/runner/jobs/work/ok', exit_code: 0, duration_ms: 90_000, outputs_dir: OUTPUTS_DIR, created_at: at(-30 * MINUTE), started_at: at(-29 * MINUTE), finished_at: at(-28 * MINUTE) }, 'all steps passed\n');

  const feed = new Feed(api, now - 40 * MINUTE);
  feed.input('Прогони полную проверку на свободном раннере');
  const run = api.id();
  feed.text(run, 'Ставлю проверку на раннер.');
  feed.call(run, 'runners', 'mcp__canterlot__runners_list', {}, hubText({ count: 2, waiting_runner_jobs: 1, runners: [
    { name: carrier.name, role: 'local', availability: 'online', observation_age_seconds: null, rank: 0, labels: [], paused: false, carrier: true, slots_used: 0, slots_max: 2, queued: 0, running: 0 },
    { name: runner.name, role: 'runner', availability: 'online', observation_age_seconds: null, rank: 20, labels: ['build-host'], paused: false, carrier: false, slots_used: 1, slots_max: 4, queued: 0, running: 1 }
  ] }));
  feed.call(run, 'flow', 'mcp__canterlot__job_run', { ...AUTO, title: 'Полная проверка' }, hubText({ job_id: flow.id, state: 'waiting_runner', machine: null, waiting_runner: true }, 'The result arrives as a [canterlot] message.'));
  feed.call(run, 'stagingFailed', 'mcp__canterlot__job_run', { ...AUTO, title: 'Проверка без кода' }, hubText({ job_id: stagingFailed.id, state: 'failed', machine: runner.name, exit_code: null, duration_ms: null, output_tail: '', output_bytes: 0, truncated: false, reason: 'staging_failed: git push failed' }));
  feed.call(run, 'notCollected', 'mcp__canterlot__job_run', { ...AUTO, title: 'Проверка без результатов' }, hubText({ job_id: notCollected.id, state: 'succeeded', machine: runner.name, exit_code: 0, duration_ms: 61_000, output_tail: 'all steps passed\n', output_bytes: 17, truncated: false, outputs_note: 'outputs not collected: collection timed out' }));
  feed.call(run, 'collected', 'mcp__canterlot__job_run', { ...AUTO, title: 'Проверка с результатами' }, hubText({ job_id: collected.id, state: 'succeeded', machine: runner.name, exit_code: 0, duration_ms: 90_000, output_tail: 'all steps passed\n', output_bytes: 17, truncated: false, outputs_dir: OUTPUTS_DIR }));
  feed.text(run, 'Проверки поставлены.');

  api.histories.set(id, feed.items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, carrier: { id: carrier.id, name: carrier.name }, runner: { id: runner.id, name: runner.name }, flow, stagingFailed, notCollected, collected, blocks: feed.blocks };
}
