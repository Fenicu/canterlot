import { describe, expect, it } from 'vitest';
import { FakeHub, FakeHubApi, FakeClock, seedMachine } from '../../../testing/fake-hub';
import { MachinesStore } from '#lib/stores/machines.svelte.js';
import type { ConnectionManager } from '#lib/sync/connection.js';

const ID = '01a1b3b9-0000-7000-8000-00000000cc01';

function view(patch: Record<string, unknown> = {}) {
	return {
		id: ID, state: 'queued', machine_id: 'm1', machine_name: 'machine-b', conversation_id: null, conversation_title: null, title: 'Сборка', workdir: '/h/u/proj',
		shell: 'npm run build', lease: 'none', timeout_minutes: 30, exit_code: null, duration_ms: null, output_bytes: 0, truncated: false, end_reason: null,
		cancel_requested: false, has_log: false, machine_offline: false, late_report: null, created_at: '2026-11-21T10:00:00Z', started_at: null, finished_at: null, ...patch
	};
}

function setup() {
	const api = new FakeHubApi(new FakeHub());
	const machine = seedMachine(api, {});
	const store = new MachinesStore({
		connection: {} as ConnectionManager,
		clock: new FakeClock(1),
		api: { snapshot: () => api.machinesSnapshot(), events: async () => ({ events: [], head: '0' }) as never }
	});
	store.applier.applySnapshot({ items: [machine] }, '1');
	const apply = (job: unknown) => store.applier.applyEvents([{ event: { kind: 'job_changed', payload: { job } } }] as never);
	return { store, apply };
}

describe('machines store: job_changed', () => {
	it('stores_the_job_and_does_not_count_it_as_an_unknown_kind', () => {
		const { store, apply } = setup();
		apply(view());
		expect(store.jobs.get(ID)?.state).toBe('queued');
		expect(store.unknownKinds).toBe(0);
	});

	it('follows_the_job_forward_to_a_final_state', () => {
		const { store, apply } = setup();
		apply(view());
		apply(view({ state: 'running', started_at: '2026-11-21T10:00:05Z' }));
		expect(store.jobs.get(ID)?.state).toBe('running');
		apply(view({ state: 'succeeded', exit_code: 0, duration_ms: 4200, finished_at: '2026-11-21T10:00:09Z' }));
		expect(store.jobs.get(ID)).toMatchObject({ state: 'succeeded', exit_code: 0, duration_ms: 4200 });
	});

	it('never_moves_a_job_back_when_an_older_copy_arrives', () => {
		const { store, apply } = setup();
		apply(view({ state: 'failed', exit_code: 1 }));
		apply(view({ state: 'running' }));
		expect(store.jobs.get(ID)?.state).toBe('failed');
		store.mergeJobs([view({ state: 'queued' })] as never);
		expect(store.jobs.get(ID)?.state).toBe('failed');
	});

	it('a_late_report_on_a_lost_job_updates_the_row_but_keeps_the_state', () => {
		const { store, apply } = setup();
		apply(view({ state: 'lost' }));
		apply(view({ state: 'lost', late_report: { outcome: 'succeeded' } }));
		expect(store.jobs.get(ID)).toMatchObject({ state: 'lost', late_report: { outcome: 'succeeded' } });
	});

	it('merges_a_fetched_list_without_dropping_jobs_the_events_brought', () => {
		const { store, apply } = setup();
		apply(view({ id: 'a', state: 'running' }));
		store.mergeJobs([view({ id: 'b', state: 'succeeded' })] as never);
		expect([...store.jobs.keys()].sort()).toEqual(['a', 'b']);
	});

	it('keeps_the_jobs_after_a_snapshot_reload_and_asks_screens_to_fetch_them_again', () => {
		const { store, apply } = setup();
		apply(view());
		const before = store.jobsEpoch;
		store.applier.applySnapshot({ items: [] }, '2');
		expect(store.jobs.size).toBe(1);
		expect(store.jobsEpoch).toBe(before + 1);
	});
});
