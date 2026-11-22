import { describe, expect, it } from 'vitest';
import { cardCall } from '../../../../src/lib/timeline/cards/call';
import type { JobCallData, JobListData, JobLogData, RunnersListData } from '../../../../src/lib/timeline/cards/defs/jobs';
import { cardIds, resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { callBlock, hub, made, resultBlock } from './helpers';

const NOW = Date.parse('2026-11-21T10:00:00Z');
const ID = '01a1b3b9-0000-7000-8000-00000000cc01';
const OTHER = '01a1b3b9-0000-7000-8000-00000000cc02';
const RUN = { machine: 'machine-b', workdir: '/h/u/proj', shell: 'npm run build', title: 'Сборка веба', wait_seconds: 20 };

function card<D>(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: D }) | null;
}

function failed(tool: string, input: unknown, output: string, root = true) {
  return cardCall(callBlock(tool, input), resultBlock(output, { summary: 'error' }), { root });
}

const finished = (patch: Record<string, unknown> = {}) => ({ job_id: ID, state: 'succeeded', machine: 'machine-b', exit_code: 0, duration_ms: 4200, output_tail: 'built\n', output_bytes: 6, truncated: false, ...patch });

describe('the registry', () => {
  it('has_a_card_for_every_job_tool', () => {
    for (const tool of ['job_run', 'job_status', 'job_log', 'job_cancel', 'jobs_list']) expect(cardIds()).toContain(`canterlot.${tool}`);
  });
});

describe('job_run', () => {
  it('a_finished_call_names_machine_and_title_and_carries_exit_code_and_duration', () => {
    const resolved = card<JobCallData>(made('mcp__canterlot__job_run', RUN, hub(finished())))!;
    expect(resolved.line).toBe('Запустила команду на machine-b: Сборка веба');
    expect(resolved.word).toEqual({ word: 'готово', tone: 'success' });
    expect(resolved.link).toEqual({ text: 'Сборка веба', href: `/machines?job=${ID}` });
    expect(resolved.data).toMatchObject({ jobId: ID, state: 'succeeded', machine: 'machine-b', exitCode: 0, durationMs: 4200, outputTail: 'built\n', refusal: null });
  });

  it('speaks_neutrally_for_a_delegate', () => {
    const call = cardCall(callBlock('mcp__canterlot__job_run', RUN), resultBlock(hub(finished())), { root: false });
    expect(card<JobCallData>(call)!.line).toBe('Команда на machine-b: Сборка веба');
  });

  it('shows_the_start_of_the_command_when_there_is_no_title', () => {
    const input = { workdir: '/h/u/proj', shell: 'cd /h/u/proj && make test\nmake lint' };
    const resolved = card<JobCallData>(made('mcp__canterlot__job_run', input, hub(finished({ machine: 'machine-a' }))))!;
    expect(resolved.line).toBe('Запустила команду на machine-a: cd /h/u/proj && make test');
  });

  it('a_failure_says_the_exit_code_and_a_timeout_says_so', () => {
    const failure = card<JobCallData>(made('mcp__canterlot__job_run', RUN, hub(finished({ state: 'failed', exit_code: 2, output_tail: 'boom' }))))!;
    expect(failure.word).toEqual({ word: 'код 2', tone: 'danger' });
    const timeout = card<JobCallData>(made('mcp__canterlot__job_run', RUN, hub(finished({ state: 'timed_out', exit_code: null, reason: 'timeout' }))))!;
    expect(timeout.word).toEqual({ word: 'время вышло', tone: 'danger' });
    expect(timeout.data.exitCode).toBeNull();
  });

  it('a_queued_or_running_answer_keeps_the_job_for_a_live_update', () => {
    const queued = card<JobCallData>(made('mcp__canterlot__job_run', RUN, hub({ job_id: ID, state: 'queued', machine: 'machine-b' }, 'Result arrives as a message')))!;
    expect(queued.word).toEqual({ word: 'в очереди', tone: 'muted' });
    expect(queued.data).toMatchObject({ jobId: ID, state: 'queued', exitCode: null, durationMs: null, outputTail: '' });
    const running = card<JobCallData>(made('mcp__canterlot__job_run', RUN, hub({ job_id: ID, state: 'running', machine: 'machine-b', machine_offline: true })))!;
    expect(running.word).toMatchObject({ word: 'идёт', live: true });
    expect(running.data.offline).toBe(true);
  });

  describe('on_a_runner', () => {
    const AUTO = { machine: 'auto', shell: 'scripts/gate.sh', title: 'Полная проверка', source: { repo: 'project', rev: '1a2b3c4' }, labels: ['kernel-6.12'], outputs: ['result'] };

    it('a_job_waiting_for_a_runner_does_not_call_auto_a_machine', () => {
      const waiting = card<JobCallData>(made('mcp__canterlot__job_run', AUTO, hub({ job_id: ID, state: 'waiting_runner', machine: null, waiting_runner: true }, 'No runner is free')))!;
      expect(waiting.line).toBe('Запустила команду: Полная проверка');
      expect(waiting.word).toEqual({ word: 'ждёт раннер', tone: 'warning' });
      expect(waiting.data).toMatchObject({ jobId: ID, state: 'waiting_runner', machine: '', auto: true, source: { repo: 'project', rev: '1a2b3c4' } });
      const asking = card<JobCallData>(made('mcp__canterlot__job_run', AUTO, null, { working: true }))!;
      expect(asking.line).toBe('Запускает команду: Полная проверка');
    });

    it('a_placed_job_names_the_runner_the_hub_chose', () => {
      const placed = card<JobCallData>(made('mcp__canterlot__job_run', AUTO, hub({ job_id: ID, state: 'staging', machine: 'runner-a' })))!;
      expect(placed.line).toBe('Запустила команду на runner-a: Полная проверка');
      expect(placed.word).toEqual({ word: 'подвозит код', tone: 'accent', live: true });
      expect(placed.data.auto).toBe(true);
    });

    it('a_finished_answer_carries_the_outputs_dir_and_the_note', () => {
      const done = card<JobCallData>(made('mcp__canterlot__job_run', AUTO, hub(finished({ machine: 'runner-a', outputs_dir: '/srv/jobs/outputs/j1', outputs_note: 'outputs truncated at the size cap' }))))!;
      expect(done.data).toMatchObject({ outputsDir: '/srv/jobs/outputs/j1', outputsNote: 'outputs truncated at the size cap' });
      const plain = card<JobCallData>(made('mcp__canterlot__job_run', RUN, hub(finished())))!;
      expect(plain.data).toMatchObject({ auto: false, source: null, outputsDir: '', outputsNote: '' });
    });

    it('the_list_row_of_a_waiting_job_has_no_machine', () => {
      const listed = card<JobListData>(made('mcp__canterlot__jobs_list', {}, hub({ jobs: [{ job_id: ID, state: 'waiting_runner', machine: null, title: 'Полная проверка', shell: 'scripts/gate.sh', created_at: '2026-11-21T10:00:00Z' }], count: 1 })))!;
      expect(listed.data.rows[0]).toMatchObject({ state: 'waiting_runner', machine: '' });
    });
  });

  it('a_call_still_waiting_for_its_answer_says_it_is_starting', () => {
    const resolved = card<JobCallData>(made('mcp__canterlot__job_run', RUN, null, { working: true }))!;
    expect(resolved.line).toBe('Запускает команду на machine-b: Сборка веба');
    expect(resolved.word).toMatchObject({ word: 'идёт' });
    const delegate = cardCall(callBlock('mcp__canterlot__job_run', RUN), null, { root: false, working: true });
    expect(card(delegate)!.line).toBe('Запуск команды на machine-b: Сборка веба');
  });

  it.each([
    ['mode_too_narrow', 'job_run needs bypass permission mode in the calling session', 'Отказ: режим не позволяет'],
    ['domain_mismatch', 'The working directory belongs to another memory domain', 'Отказ: другой домен памяти'],
    ['remote_work_disabled', 'The machine does not accept remote work', 'Машина не принимает работу с других машин'],
    ['capability_missing', 'The worker does not support this yet', 'Воркер машины устарел'],
    ['workdir_not_allowed', 'Pass a workdir under the allowed roots', 'Каталог не подходит'],
    ['job_not_found', 'No such job in this session', 'Отказ: job_not_found']
  ])('a_refusal_%s_reads_in_words', (code, text, line) => {
    const resolved = card<JobCallData>(failed('mcp__canterlot__job_run', RUN, `${code}: ${text}`))!;
    expect(resolved.line).toBe(line);
    expect(resolved.data.refusal).toBe(code);
    expect(resolved.word).toMatchObject({ word: 'ошибка', tone: 'danger' });
    expect(resolved.link).toBeNull();
  });

  it('a_failed_call_without_a_code_still_reads_as_a_refusal', () => {
    expect(card<JobCallData>(failed('mcp__canterlot__job_run', RUN, 'something broke'))!.line).toBe('Отказ: вызов не удался');
  });

  it('without_an_answer_the_feed_shows_the_plain_block', () => {
    expect(card(made('mcp__canterlot__job_run', RUN, ''))).toBeNull();
    expect(card(made('mcp__canterlot__job_run', RUN, 'not json at all'))).toBeNull();
    expect(card(made('mcp__canterlot__job_run', RUN, hub({ unrelated: true })))).toBeNull();
  });
});

describe('job_status', () => {
  it('reads_the_state_of_the_job_in_both_voices', () => {
    const answer = hub(finished({ state: 'failed', exit_code: 1 }));
    const she = card<JobCallData>(made('mcp__canterlot__job_status', { job_id: ID }, answer))!;
    expect(she.line).toBe('Проверила задачу 01a1b3b9 на machine-b');
    expect(she.word).toEqual({ word: 'код 1', tone: 'danger' });
    expect(she.link).toEqual({ text: '01a1b3b9', href: `/machines?job=${ID}` });
    const delegate = card<JobCallData>(cardCall(callBlock('mcp__canterlot__job_status', { job_id: ID }), resultBlock(answer), { root: false }))!;
    expect(delegate.line).toBe('Состояние задачи 01a1b3b9 на machine-b');
  });

  it('keeps_the_hub_marks_of_a_lost_job', () => {
    const resolved = card<JobCallData>(made('mcp__canterlot__job_status', { job_id: ID }, hub({ job_id: ID, state: 'lost', machine: 'machine-b', late_report: { outcome: 'succeeded' } })))!;
    expect(resolved.word).toEqual({ word: 'потеряна', tone: 'danger' });
    expect(resolved.data.lateReport).toBe(true);
  });

  it('refusals_and_a_missing_answer', () => {
    expect(card<JobCallData>(failed('mcp__canterlot__job_status', { job_id: ID }, 'job_not_found: No such job in this session'))!.line).toBe('Отказ: job_not_found');
    expect(card(made('mcp__canterlot__job_status', { job_id: ID }, ''))).toBeNull();
    expect(card(made('mcp__canterlot__job_status', {}, hub({ state: 'running' })))).toBeNull();
  });
});

describe('job_log', () => {
  const slice = (patch: Record<string, unknown> = {}) => ({ job_id: ID, state: 'running', source: 'tail', offset: 0, next_offset: 12, total_bytes: 12, eof: false, text: 'step 1\nstep 2', note: null, ...patch });

  it('reads_a_piece_of_the_log_in_both_voices', () => {
    const she = card<JobLogData>(made('mcp__canterlot__job_log', { job_id: ID }, hub(slice())))!;
    expect(she.line).toBe('Читала журнал задачи 01a1b3b9');
    expect(she.word).toMatchObject({ word: 'идёт', tone: 'accent' });
    expect(she.data).toMatchObject({ jobId: ID, text: 'step 1\nstep 2', source: 'tail', expired: false });
    const delegate = card<JobLogData>(cardCall(callBlock('mcp__canterlot__job_log', { job_id: ID }), resultBlock(hub(slice())), { root: false }))!;
    expect(delegate.line).toBe('Журнал задачи 01a1b3b9');
  });

  it('an_expired_full_log_is_marked', () => {
    const resolved = card<JobLogData>(made('mcp__canterlot__job_log', { job_id: ID }, hub(slice({ state: 'succeeded', note: 'full log expired' }))))!;
    expect(resolved.data.expired).toBe(true);
  });

  it('refusals_and_a_missing_answer', () => {
    expect(card<JobLogData>(failed('mcp__canterlot__job_log', { job_id: ID }, 'job_not_found: x'))!.line).toBe('Отказ: job_not_found');
    expect(card(made('mcp__canterlot__job_log', { job_id: ID }, 'plain text'))).toBeNull();
  });
});

describe('job_cancel', () => {
  it('a_canceled_job_says_so_in_both_voices', () => {
    const answer = hub(finished({ state: 'canceled', exit_code: null, reason: 'canceled_by_caller' }));
    const she = card<JobCallData>(made('mcp__canterlot__job_cancel', { job_id: ID }, answer))!;
    expect(she.line).toBe('Отменила задачу 01a1b3b9 на machine-b');
    expect(she.word).toEqual({ word: 'отменена', tone: 'muted' });
    const delegate = card<JobCallData>(cardCall(callBlock('mcp__canterlot__job_cancel', { job_id: ID }), resultBlock(answer), { root: false }))!;
    expect(delegate.line).toBe('Отмена задачи 01a1b3b9 на machine-b');
  });

  it('a_running_job_only_gets_the_request', () => {
    const she = card<JobCallData>(made('mcp__canterlot__job_cancel', { job_id: ID }, hub({ job_id: ID, state: 'running', machine: 'machine-b', cancel_requested: true })))!;
    expect(she.line).toBe('Попросила отменить задачу 01a1b3b9 на machine-b');
    expect(she.word).toMatchObject({ word: 'отменяется', tone: 'warning' });
    expect(she.data.cancelRequested).toBe(true);
  });

  it('refusals_and_a_missing_answer', () => {
    expect(card<JobCallData>(failed('mcp__canterlot__job_cancel', { job_id: ID }, 'job_not_found: x'))!.line).toBe('Отказ: job_not_found');
    expect(card(made('mcp__canterlot__job_cancel', { job_id: ID }, ''))).toBeNull();
  });
});

describe('runners_list', () => {
  const runner = (patch: Record<string, unknown> = {}) => ({ name: 'runner-a', role: 'runner', availability: 'online', observation_age_seconds: null, rank: 20, labels: ['kernel-6.12'], paused: false, carrier: false, slots_used: 1, slots_max: 4, queued: 0, running: 1, ...patch });
  const answer = hub({ count: 3, waiting_runner_jobs: 2, runners: [runner({ name: 'carrier', role: 'local', rank: 0, carrier: true, slots_used: 0, slots_max: 2 }), runner(), runner({ name: 'runner-b', availability: 'offline', observation_age_seconds: 130, slots_used: 0, paused: true })] });

  it('is_a_card_of_its_own', () => {
    expect(cardIds()).toContain('canterlot.runners_list');
  });

  it('counts_the_runners_on_the_line_in_both_voices', () => {
    const she = card<RunnersListData>(made('mcp__canterlot__runners_list', {}, answer))!;
    expect(she.line).toBe('Смотрела раннеры: 3 · 2 на связи · ждут 2');
    const delegate = cardCall(callBlock('mcp__canterlot__runners_list', {}), resultBlock(answer), { root: false });
    expect(card<RunnersListData>(delegate)!.line).toBe('Раннеры: 3 · 2 на связи · ждут 2');
  });

  it('keeps_name_availability_and_slots_of_each_row', () => {
    const rows = card<RunnersListData>(made('mcp__canterlot__runners_list', {}, answer))!.data.rows;
    expect(rows.map((row) => [row.name, row.availability, row.slotsUsed, row.slotsMax, row.paused, row.local])).toEqual([
      ['carrier', 'online', 0, 2, false, true],
      ['runner-a', 'online', 1, 4, false, false],
      ['runner-b', 'offline', 0, 4, true, false]
    ]);
  });

  it('drops_rows_without_a_name_and_shows_the_plain_block_without_an_answer', () => {
    expect(card<RunnersListData>(made('mcp__canterlot__runners_list', {}, hub({ count: 2, runners: [runner(), { slots_used: 1 }] })))!.data.rows).toHaveLength(1);
    expect(card(made('mcp__canterlot__runners_list', {}, ''))).toBeNull();
    expect(card<RunnersListData>(made('mcp__canterlot__runners_list', {}, hub({ count: 0, runners: [] })))!.line).toBe('Смотрела раннеры: нет');
  });
});

describe('jobs_list', () => {
  const row = (patch: Record<string, unknown> = {}) => ({ job_id: ID, state: 'running', machine: 'machine-b', title: 'Сборка веба', shell: 'npm run build', exit_code: null, duration_ms: null, created_at: '2026-11-21T09:59:00Z', machine_offline: false, ...patch });

  it('counts_the_jobs_and_the_unfinished_ones_in_both_voices', () => {
    const answer = hub({ count: 3, jobs: [row(), row({ job_id: OTHER, state: 'succeeded', exit_code: 0, duration_ms: 900, title: null }), row({ job_id: 'x', state: 'queued' })] });
    const she = card<JobListData>(made('mcp__canterlot__jobs_list', {}, answer))!;
    expect(she.line).toBe('Смотрела задачи: 3 · 1 идёт, 1 в очереди');
    expect(she.data.rows.map((item) => [item.jobId, item.state, item.machine, item.title, item.shell])).toEqual([
      [ID, 'running', 'machine-b', 'Сборка веба', 'npm run build'],
      [OTHER, 'succeeded', 'machine-b', '', 'npm run build'],
      ['x', 'queued', 'machine-b', 'Сборка веба', 'npm run build']
    ]);
    const delegate = card<JobListData>(cardCall(callBlock('mcp__canterlot__jobs_list', {}), resultBlock(answer), { root: false }))!;
    expect(delegate.line).toBe('Задачи: 3 · 1 идёт, 1 в очереди');
  });

  it('an_empty_list_says_there_are_no_jobs', () => {
    expect(card<JobListData>(made('mcp__canterlot__jobs_list', {}, hub({ count: 0, jobs: [] })))!.line).toBe('Смотрела задачи: нет');
  });

  it('drops_rows_without_an_id_and_shows_the_plain_block_without_an_answer', () => {
    expect(card<JobListData>(made('mcp__canterlot__jobs_list', {}, hub({ count: 2, jobs: [row(), { state: 'queued' }] })))!.data.rows).toHaveLength(1);
    expect(card(made('mcp__canterlot__jobs_list', {}, ''))).toBeNull();
    expect(card(made('mcp__canterlot__jobs_list', {}, hub({ other: 1 })))).toBeNull();
  });

  it('a_refusal_reads_in_words', () => {
    expect(card<JobListData>(failed('mcp__canterlot__jobs_list', { machine: 'nowhere' }, 'machine_not_found: x'))!.line).toBe('Отказ: machine_not_found');
  });
});
