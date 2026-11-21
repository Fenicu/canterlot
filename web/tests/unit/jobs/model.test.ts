import { describe, expect, it } from 'vitest';
import { durationWord, isTerminal, jobFacts, jobHref, jobLabel, jobWord, machineJobs, offlineNote, outputsLine, refusalText, reasonText, runnerLine, sourceLine, type JobFacts, type JobView } from '../../../src/lib/jobs/model';

const job = (patch: Partial<JobFacts> = {}): JobFacts => ({
  id: '01a1b3b9-0000-7000-8000-00000000cc01',
  state: 'running',
  machineId: 'm1',
  machineName: 'machine-b',
  title: null,
  shell: 'npm test',
  workdir: '/h/u/proj',
  exitCode: null,
  durationMs: null,
  cancelRequested: false,
  machineOffline: false,
  endReason: null,
  createdAt: '2026-11-21T10:00:00Z',
  labels: [],
  expectedMinutes: 30,
  source: null,
  outputsDir: null,
  outputsNote: null,
  ...patch
});

describe('job words', () => {
  it('names_every_state_in_russian_with_a_tone', () => {
    const word = (state: string, patch: Partial<JobFacts> = {}) => jobWord(job({ state, ...patch }));
    expect(word('waiting_runner')).toEqual({ word: 'ждёт раннер', tone: 'warning' });
    expect(word('staging')).toEqual({ word: 'подвозит код', tone: 'accent', live: true });
    expect(word('collecting')).toEqual({ word: 'собирает результаты', tone: 'accent', live: true });
    expect(isTerminal('collecting')).toBe(false);
    expect(refusalText('source_required')).toBe('Для автоматического размещения нужны исходники');
    expect(word('queued')).toEqual({ word: 'в очереди', tone: 'muted' });
    expect(word('dispatched')).toEqual({ word: 'передана машине', tone: 'muted' });
    expect(word('waiting_lease')).toEqual({ word: 'ждёт очереди сборок', tone: 'warning' });
    expect(word('running')).toEqual({ word: 'идёт', tone: 'accent', live: true });
    expect(word('succeeded', { exitCode: 0 })).toEqual({ word: 'готово', tone: 'success' });
    expect(word('failed', { exitCode: 2 })).toEqual({ word: 'код 2', tone: 'danger' });
    expect(word('failed')).toEqual({ word: 'ошибка', tone: 'danger' });
    expect(word('timed_out')).toEqual({ word: 'время вышло', tone: 'danger' });
    expect(word('canceled')).toEqual({ word: 'отменена', tone: 'muted' });
    expect(word('lost')).toEqual({ word: 'потеряна', tone: 'danger' });
  });

  it('a_running_job_with_a_cancel_request_says_it_is_being_canceled', () => {
    expect(jobWord(job({ cancelRequested: true }))).toEqual({ word: 'отменяется', tone: 'warning', live: true });
  });

  it('an_unknown_state_is_shown_as_it_came', () => {
    expect(jobWord(job({ state: 'strange' }))).toEqual({ word: 'strange', tone: 'muted' });
  });

  it('tells_terminal_states_apart', () => {
    expect(['succeeded', 'failed', 'timed_out', 'canceled', 'lost'].every(isTerminal)).toBe(true);
    expect(['queued', 'dispatched', 'waiting_lease', 'running'].some(isTerminal)).toBe(false);
  });

  it('a_lost_job_reads_connection_lost_and_an_offline_running_one_says_the_machine_is_away', () => {
    expect(offlineNote(job({ state: 'lost' }))).toBe('связь с машиной потеряна');
    expect(offlineNote(job({ machineOffline: true }))).toBe('машина не на связи');
    expect(offlineNote(job({ state: 'succeeded', machineOffline: true }))).toBe('');
    expect(offlineNote(job())).toBe('');
  });
});

describe('job text', () => {
  it('shortens_a_duration_like_the_rest_of_the_feed', () => {
    expect(durationWord(null)).toBe('');
    expect(durationWord(320)).toBe('0,3 с');
    expect(durationWord(65_000)).toBe('1 мин 5 с');
  });

  it('labels_a_job_by_title_or_the_first_line_of_the_command', () => {
    expect(jobLabel('Сборка', 'npm run build')).toBe('Сборка');
    expect(jobLabel('', 'cd /h/u/proj && npm run build\nnpm test')).toBe('cd /h/u/proj && npm run build');
    expect(jobLabel(null, '  ')).toBe('');
    expect(jobLabel(null, 'x'.repeat(200)).length).toBeLessThanOrEqual(80);
  });

  it('links_the_job_sheet_on_the_machines_screen', () => {
    expect(jobHref('abc')).toBe('/machines?job=abc');
  });

  it('words_every_refusal_and_falls_back_to_the_code', () => {
    expect(refusalText('mode_too_narrow')).toBe('Отказ: режим не позволяет');
    expect(refusalText('domain_mismatch')).toBe('Отказ: другой домен памяти');
    expect(refusalText('remote_work_disabled')).toBe('Машина не принимает работу с других машин');
    expect(refusalText('capability_missing')).toBe('Воркер машины устарел');
    expect(refusalText('workdir_not_allowed')).toBe('Каталог не подходит');
    expect(refusalText('workdir_refused')).toBe('Каталог не подходит');
    expect(refusalText('weird_code')).toBe('Отказ: weird_code');
    expect(refusalText('')).toBe('Отказ: вызов не удался');
  });

  it('words_the_end_reasons_the_hub_stores', () => {
    expect(reasonText('canceled_by_owner')).toBe('отменена владельцем');
    expect(reasonText('canceled_by_caller')).toBe('отменена агентом');
    expect(reasonText('launch_interrupted')).toBe('запуск прерван сбоем машины');
    expect(reasonText('workdir_refused')).toBe('Каталог не подходит');
    expect(reasonText('mode_too_narrow')).toBe('Отказ: режим не позволяет');
    expect(reasonText(null)).toBe('');
  });
});

describe('machine jobs', () => {
  const at = (n: number) => `2026-11-21T10:${String(n).padStart(2, '0')}:00Z`;
  it('keeps_every_unfinished_job_and_the_last_ten_finished_ones_newest_first', () => {
    const done = Array.from({ length: 12 }, (_, i) => job({ id: `d${i}`, state: 'succeeded', createdAt: at(i) }));
    const live = [job({ id: 'r1', state: 'running', createdAt: at(2) }), job({ id: 'q1', state: 'queued', createdAt: at(30) })];
    const other = job({ id: 'o1', machineId: 'm2', state: 'running', createdAt: at(40) });
    const rows = machineJobs([...done, ...live, other], 'm1');
    expect(rows.running.map((row) => row.id)).toEqual(['q1', 'r1']);
    expect(rows.recent.map((row) => row.id)).toEqual(['d11', 'd10', 'd9', 'd8', 'd7', 'd6', 'd5', 'd4', 'd3', 'd2']);
  });

  it('is_empty_when_the_machine_has_no_jobs', () => {
    expect(machineJobs([], 'm1')).toEqual({ running: [], recent: [] });
  });

  it('a_carrier_shows_the_jobs_nobody_has_taken_yet_and_not_those_on_its_runners', () => {
    const runnerJob = job({ id: 'r1', machineId: 'runner-a', state: 'running', createdAt: at(5) });
    const waiting = job({ id: 'w1', machineId: '', machineName: '', state: 'waiting_runner', createdAt: at(6) });
    const ownJob = job({ id: 'c1', machineId: 'carrier', state: 'running', createdAt: at(4) });
    const canceled = job({ id: 'w2', machineId: '', machineName: '', state: 'canceled', createdAt: at(3) });
    const rows = machineJobs([runnerJob, waiting, ownJob, canceled], 'carrier', { carrier: true });
    expect(rows.running.map((row) => row.id)).toEqual(['w1', 'c1']);
    expect(rows.recent.map((row) => row.id)).toEqual(['w2']);
    // Карточка раннера держит только свои задачи; несущая без пометки не берёт чужие и ничьи.
    expect(machineJobs([runnerJob, waiting], 'runner-a').running.map((row) => row.id)).toEqual(['r1']);
    expect(machineJobs([runnerJob, waiting], 'carrier').running).toEqual([]);
  });
});

describe('runner job facts', () => {
  const view = (patch: Partial<JobView> = {}): JobView => ({
    id: 'j1', state: 'staging', machine_id: null, machine_name: null, conversation_id: null, conversation_title: null, title: null, workdir: null,
    shell: 'scripts/gate.sh', lease: 'none', timeout_minutes: 30, exit_code: null, duration_ms: null, output_bytes: 0, truncated: false, end_reason: null,
    cancel_requested: false, has_log: false, machine_offline: false, late_report: null, labels: ['kernel-6.12'], expected_minutes: 45,
    source: { repo: 'project', rev: '1a2b3c4' }, outputs_dir: null, outputs_note: null, created_at: '2026-11-21T10:00:00Z', started_at: null, finished_at: null, ...patch
  });

  it('reads_the_runner_fields_of_the_hub_row', () => {
    const facts = jobFacts(view({ outputs_dir: '/srv/jobs/outputs/j1', outputs_note: 'cap reached' }));
    expect(facts).toMatchObject({ machineId: '', workdir: '', labels: ['kernel-6.12'], expectedMinutes: 45, source: { repo: 'project', rev: '1a2b3c4' }, outputsDir: '/srv/jobs/outputs/j1', outputsNote: 'cap reached' });
    expect(jobFacts(view({ source: null })).source).toBeNull();
  });

  it('names_the_runner_or_says_the_job_waits_for_one', () => {
    expect(runnerLine(job({ state: 'waiting_runner', machineId: '', machineName: '' }))).toBe('ждёт раннер');
    expect(runnerLine(job({ state: 'staging', machineName: 'runner-a' }))).toBe('runner-a');
    expect(runnerLine(job({ state: 'canceled', machineId: '', machineName: '' }))).toBe('');
  });

  it('shows_the_source_as_repo_at_rev', () => {
    expect(sourceLine({ repo: 'project', rev: '1a2b3c4' })).toBe('project @ 1a2b3c4');
    expect(sourceLine(null)).toBe('');
  });

  it('tells_where_the_outputs_are_only_after_the_end', () => {
    expect(outputsLine(job({ state: 'succeeded', outputsDir: '/srv/jobs/outputs/j1' }))).toEqual({ dir: '/srv/jobs/outputs/j1', note: '' });
    expect(outputsLine(job({ state: 'failed', outputsDir: null, outputsNote: 'outputs not collected: collection timed out' }))).toEqual({ dir: '', note: 'Результаты не собраны: collection timed out' });
    expect(outputsLine(job({ state: 'failed', outputsDir: null, outputsNote: 'outputs not collected' }))).toEqual({ dir: '', note: 'Результаты не собраны' });
    expect(outputsLine(job({ state: 'succeeded', outputsDir: '/srv/jobs/outputs/j1', outputsNote: 'outputs truncated at the size cap' }))).toEqual({ dir: '/srv/jobs/outputs/j1', note: 'Результаты обрезаны по пределу размера' });
    expect(outputsLine(job({ state: 'succeeded', outputsDir: null, outputsNote: 'something new' }))).toEqual({ dir: '', note: 'something new' });
    expect(outputsLine(job({ state: 'collecting', outputsDir: '/srv/jobs/outputs/j1' }))).toBeNull();
    expect(outputsLine(job({ state: 'succeeded' }))).toBeNull();
  });
});
