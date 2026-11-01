import { describe, expect, it } from 'vitest';
import { BackgroundModel, type BackgroundTask } from '#lib/stores/background.svelte.js';
import { BlockModel } from '#lib/stores/blocks.js';
import { buildDelegation } from '#lib/stores/delegation.js';

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

describe('delegation state', () => {
	const call = (): BlockModel => {
		const block = new BlockModel('b1', 'tool_call');
		block.toolName = 'Agent';
		block.state = 'final';
		block.createdSeq = '1';
		block.createdAt = 1000;
		block.finishedAt = 1000;
		return block;
	};
	const result = (summary: string | null): BlockModel => {
		const block = new BlockModel('r1', 'tool_result');
		block.parentId = 'b1';
		block.state = 'final';
		block.createdSeq = '2';
		block.summary = summary;
		block.createdAt = 3000;
		block.finishedAt = 3000;
		return block;
	};
	const none = new Map();

	it('builddelegation_keeps_a_launched_agent_running', () => {
		const facts = { background: new Map([['b1', { state: 'running', kind: 'agent', endedAt: null }]]), live: true };
		const [node] = buildDelegation([call(), result('async_launched')], facts);
		expect(node).toMatchObject({ state: 'running', finished_at: null });
	});

	it('synchronous_agent_runs_until_its_real_result', () => {
		const facts = { background: none, live: true };
		expect(buildDelegation([call()], facts)[0]?.state).toBe('running');
		const [node] = buildDelegation([call(), result(null)], facts);
		expect(node?.state).toBe('done');
		expect(node?.finished_at).toBe(new Date(3000).toISOString());
		expect(buildDelegation([call(), result('error')], facts)[0]?.state).toBe('failed');
	});

	it('call_without_a_result_of_a_dead_execution_is_incomplete', () => {
		expect(buildDelegation([call()], { background: none, live: false })[0]?.state).toBe('incomplete');
	});

	it('background_end_decides', () => {
		const facts = { background: new Map([['b1', { state: 'failed', kind: 'agent', endedAt: 5000 }]]), live: true };
		expect(buildDelegation([call(), result('async_launched')], facts)[0]).toMatchObject({
			state: 'failed',
			finished_at: new Date(5000).toISOString()
		});
	});

	it('names_the_subagent_by_its_own_blocks_not_by_the_launch_result', () => {
		const main = call();
		main.agent = 'orchestrator';
		main.model = 'claude-opus-5-5';
		const launch = result('async_launched');
		launch.agent = 'orchestrator';
		launch.model = 'claude-opus-5-5';
		const inner = new BlockModel('i1', 'tool_call');
		inner.parentId = 'b1';
		inner.agent = 'general-purpose';
		inner.model = 'claude-sonnet-5-5';
		inner.createdSeq = '3';
		expect(buildDelegation([main, launch])[0]?.subagent).toBeNull();
		expect(buildDelegation([main, launch, inner])[0]?.subagent).toEqual({
			name: 'general-purpose',
			model: 'claude-sonnet-5-5'
		});
	});

	it('other_harnesses_keep_the_call_state', () => {
		expect(buildDelegation([call()])[0]?.state).toBe('done');
	});
});
