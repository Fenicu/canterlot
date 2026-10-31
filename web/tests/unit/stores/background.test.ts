import { describe, expect, it } from 'vitest';
import { BackgroundModel, type BackgroundTask } from '#lib/stores/background.svelte.js';

function task(over: Partial<BackgroundTask> = {}): BackgroundTask {
	return {
		task_id: 'a1',
		kind: 'agent',
		block_id: 'b1',
		parent_task_id: null,
		description: 'bg sub',
		state: 'running',
		started_at: '2026-11-01T00:00:00.000Z',
		ended_at: null,
		...over
	};
}

const ended = { state: 'completed', ended_at: '2026-11-01T00:01:00.000Z' };

describe('stores/background', () => {
	it('latest_event_wins_and_a_task_can_run_again', () => {
		const model = new BackgroundModel();
		model.apply([task()]);
		model.apply([task(ended)]);
		expect(model.facts(true).get('b1')?.state).toBe('completed');
		model.apply([task({ started_at: '2026-11-01T00:02:00.000Z' })]);
		expect(model.facts(true).get('b1')).toMatchObject({ state: 'running', endedAt: null });
	});

	it('snapshot_does_not_undo_a_newer_event', () => {
		const model = new BackgroundModel();
		model.apply([task(ended)]);
		model.ingestSnapshot([task()]);
		expect(model.facts(true).get('b1')?.state).toBe('completed');
	});

	it('running_task_of_a_dead_execution_is_stopped', () => {
		const model = new BackgroundModel();
		model.apply([task()]);
		expect(model.facts(false).get('b1')?.state).toBe('stopped');
	});

	it('task_without_a_call_has_no_fact', () => {
		const model = new BackgroundModel();
		model.apply([task({ block_id: null })]);
		expect(model.facts(true).size).toBe(0);
	});
});
