import { describe, expect, it } from 'vitest';
import { BlockModel } from '#lib/stores/blocks.js';
import {
	groupDelegation,
	interruptedCount,
	latestTurnStart,
	mergeNodes,
	runningCount,
	type DelegationNode
} from '#lib/stores/delegation.js';

const T0 = Date.parse('2026-11-01T10:00:00.000Z');
const at = (min: number): string => new Date(T0 + min * 60_000).toISOString();

function node(id: string, state: string, start: number, end: number | null = null): DelegationNode {
	return {
		block_id: id,
		parent_block_id: null,
		started_at: at(start),
		finished_at: end === null ? null : at(end),
		state,
		tool_name: 'Agent',
		task: id,
		subagent: { name: id, model: 'opus' }
	};
}

function block(id: string, seq: number, run: string | null, min: number): BlockModel {
	const value = new BlockModel(id, 'text');
	value.createdSeq = String(seq);
	value.inputCommandId = run;
	value.createdAt = T0 + min * 60_000;
	return value;
}

describe('stores/delegation groups', () => {
	it('running_first_then_finished_in_the_latest_turn_and_the_rest_collapse', () => {
		const nodes = [
			node('old1', 'done', 0, 1),
			node('run', 'running', 5),
			node('old2', 'failed', 1, 2),
			node('fresh', 'done', 10, 11)
		];
		const groups = groupDelegation(nodes, T0 + 9 * 60_000);
		expect(groups.shown.map((n) => n.block_id)).toEqual(['run', 'fresh']);
		expect(groups.older.map((n) => n.block_id)).toEqual(['old2', 'old1']);
	});

	it('a_subagent_that_ended_before_the_turn_began_is_older_even_if_started_inside_it', () => {
		const groups = groupDelegation([node('a', 'done', 3, 4)], T0 + 5 * 60_000);
		expect(groups.shown).toEqual([]);
		expect(groups.older).toHaveLength(1);
	});

	it('a_background_subagent_launched_earlier_but_ended_in_the_turn_stays_shown', () => {
		const groups = groupDelegation([node('a', 'done', 0, 12)], T0 + 9 * 60_000);
		expect(groups.shown.map((n) => n.block_id)).toEqual(['a']);
	});

	it('running_stays_visible_even_when_started_long_ago', () => {
		const groups = groupDelegation([node('a', 'running', 0), node('w', 'waiting', 1)], T0 + 50 * 60_000);
		expect(groups.shown.map((n) => n.block_id)).toEqual(['a', 'w']);
		expect(groups.older).toEqual([]);
	});

	it('finished_without_an_end_time_is_placed_by_its_start', () => {
		const groups = groupDelegation([node('lost', 'incomplete', 12), node('lost2', 'incomplete', 1)], T0 + 9 * 60_000);
		expect(groups.shown.map((n) => n.block_id)).toEqual(['lost']);
		expect(groups.older.map((n) => n.block_id)).toEqual(['lost2']);
	});

	it('without_a_known_turn_nothing_finished_counts_as_fresh', () => {
		const groups = groupDelegation([node('a', 'done', 0, 1), node('b', 'running', 2)], null);
		expect(groups.shown.map((n) => n.block_id)).toEqual(['b']);
		expect(groups.older.map((n) => n.block_id)).toEqual(['a']);
	});

	it('older_are_newest_first_by_end_time', () => {
		const groups = groupDelegation([node('a', 'done', 0, 3), node('b', 'done', 1, 2), node('c', 'done', 2, 5)], T0 + 60 * 60_000);
		expect(groups.older.map((n) => n.block_id)).toEqual(['c', 'a', 'b']);
	});

	it('counts_interrupted_and_failed_in_the_collapsed_set', () => {
		const groups = groupDelegation(
			[node('a', 'done', 0, 1), node('b', 'stopped', 0, 1), node('c', 'failed', 0, 1), node('d', 'incomplete', 0, 1), node('e', 'error', 0, 1)],
			T0 + 60 * 60_000
		);
		expect(groups.older).toHaveLength(5);
		expect(groups.interrupted).toBe(4);
		expect(interruptedCount(groups.older)).toBe(4);
	});

	it('running_count_ignores_finished', () => {
		expect(runningCount([node('a', 'running', 0), node('b', 'done', 0, 1), node('c', 'waiting', 0), node('d', 'stopped', 0, 1)])).toBe(2);
	});

	it('latest_turn_starts_at_the_first_block_of_the_run_of_the_newest_block', () => {
		const blocks = [block('a', 1, 'r1', 0), block('b', 2, 'r1', 1), block('c', 3, 'r2', 7), block('d', 4, 'r2', 8)];
		expect(latestTurnStart(blocks)).toBe(T0 + 7 * 60_000);
	});

	it('latest_turn_is_unknown_without_runs', () => {
		expect(latestTurnStart([block('a', 1, null, 0)])).toBeNull();
		expect(latestTurnStart([])).toBeNull();
	});

	it('live_nodes_override_server_ones_by_block', () => {
		const merged = mergeNodes([node('a', 'running', 0), node('s', 'done', 0, 1)], [node('a', 'done', 0, 2)]);
		expect(merged.map((n) => [n.block_id, n.state])).toEqual([
			['a', 'done'],
			['s', 'done']
		]);
	});
});
