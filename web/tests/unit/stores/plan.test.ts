import { describe, expect, it } from 'vitest';
import { ListStore, type ListSnapshot } from '#lib/stores/list.svelte.js';
import { PlanModel, type PlanTask } from '#lib/stores/plan.svelte.js';
import type { ConnectionManager } from '#lib/sync/connection.js';
import type { HubEvent } from '#lib/sync/wire.js';

const ORCH = '00000000-0000-4000-8000-0000000000a1';
const OTHER = '00000000-0000-4000-8000-0000000000a2';

function task(patch: Partial<PlanTask> = {}): PlanTask {
	return {
		id: '00000000-0000-4000-8000-000000000101',
		orchestrator_id: ORCH,
		number: 1,
		title: 'Задача',
		note: null,
		blocker: null,
		findings: [],
		state: 'waiting',
		executor_id: null,
		attempts: 0,
		created_at: '2026-11-08T10:00:00.000Z',
		updated_at: '2026-11-08T10:00:00.000Z',
		revision: 1,
		...patch
	};
}

function ev(kind: string, payload: unknown, seq = '1'): HubEvent {
	return {
		v: 1,
		id: '00000000-0000-4000-8000-0000000000ee',
		correlation_id: '00000000-0000-4000-8000-0000000000ef',
		scope: 'conversations',
		scope_seq: seq,
		scope_seq_from: seq,
		committed_at: '2026-11-08T10:00:00.000Z',
		committed_at_from: '2026-11-08T10:00:00.000Z',
		conversation_id: null,
		execution_id: null,
		kind,
		payload
	} as HubEvent;
}

function snapshot(plan_tasks: PlanTask[]): ListSnapshot {
	return { counts: { all: 0, pinned: 0, archived: 0, trashed: 0, groups: [] }, groups: [], items: [], limits: [], truncated: false, presets: [], plan_tasks };
}

function store(): ListStore {
	return new ListStore({
		connection: {} as ConnectionManager,
		api: { snapshot: async () => ({ cursor: '0', data: snapshot([]) }), events: async () => ({ events: [], head: '0' }) }
	});
}

describe('PlanModel', () => {
	it('plan_task_apply_ignores_stale_and_equal_revisions', () => {
		const plan = new PlanModel();
		expect(plan.apply(task({ revision: 3, title: 'Новое' }))).toBe(true);
		expect(plan.apply(task({ revision: 2, title: 'Старое' }))).toBe(false);
		expect(plan.apply(task({ revision: 3, title: 'Повтор' }))).toBe(false);
		expect(plan.of(ORCH).map((item) => item.title)).toEqual(['Новое']);
		expect(plan.apply(task({ revision: 4, state: 'in_progress' }))).toBe(true);
		expect(plan.of(ORCH)[0]!.state).toBe('in_progress');
	});

	it('plan_snapshot_replaces_the_set_instead_of_merging', () => {
		const plan = new PlanModel();
		plan.apply(task({ id: 'a', revision: 9 }));
		plan.apply(task({ id: 'b', number: 2 }));
		plan.replace([task({ id: 'a', revision: 1, title: 'Из снимка' })]);
		// Снимок — правда: задача b исчезла, а a взяла ревизию снимка, даже меньшую.
		expect(plan.of(ORCH).map((item) => [item.id, item.title, item.revision])).toEqual([['a', 'Из снимка', 1]]);
	});

	it('plan_of_sorts_by_number_and_orchestrators_by_latest_change', () => {
		const plan = new PlanModel();
		plan.apply(task({ id: 'c', number: 3 }));
		plan.apply(task({ id: 'a', number: 1 }));
		plan.apply(task({ id: 'x', orchestrator_id: OTHER, updated_at: '2026-11-08T11:00:00.000Z' }));
		expect(plan.of(ORCH).map((item) => item.number)).toEqual([1, 3]);
		expect(plan.orchestrators()).toEqual([OTHER, ORCH]);
		expect(plan.has(OTHER)).toBe(true);
		plan.dropOrchestrator(OTHER);
		expect(plan.has(OTHER)).toBe(false);
		expect(plan.orchestrators()).toEqual([ORCH]);
	});

	it('plan_revision_counter_grows_per_orchestrator_on_accepted_changes_only', () => {
		const plan = new PlanModel();
		plan.apply(task({ revision: 1 }));
		const first = plan.revisionOf(ORCH);
		plan.apply(task({ revision: 1 }));
		expect(plan.revisionOf(ORCH)).toBe(first);
		plan.apply(task({ revision: 2 }));
		expect(plan.revisionOf(ORCH)).toBe(first + 1);
		expect(plan.revisionOf(OTHER)).toBe(0);
	});
});

describe('ListStore plan', () => {
	it('list_store_applies_plan_task_changed_and_snapshot_replace', () => {
		const list = store();
		list.applier.applySnapshot(snapshot([task({ id: 'a' }), task({ id: 'b', number: 2 })]), "0");
		expect(list.plan.of(ORCH)).toHaveLength(2);
		list.applier.applyEvents([{ event: ev('plan_task_changed', { task: task({ id: 'a', state: 'in_review', revision: 2 }) }) }] as never);
		expect(list.plan.of(ORCH)[0]!.state).toBe('in_review');
		list.applier.applySnapshot(snapshot([task({ id: 'b', number: 2 })]), "0");
		expect(list.plan.of(ORCH).map((item) => item.id)).toEqual(['b']);
		list.applier.applySnapshot({ ...snapshot([]), plan_tasks: undefined }, "0");
		expect(list.plan.tasks.size).toBe(0);
		expect(list.unknownKinds).toBe(0);
	});

	it('list_store_drops_plan_of_a_deleted_orchestrator_and_ignores_late_events', () => {
		const list = store();
		list.applier.applySnapshot(snapshot([task({ id: 'a' }), task({ id: 'x', orchestrator_id: OTHER })]), "0");
		list.applier.applyEvents([{ event: ev('conversation_deleted', { id: ORCH, counts: { all: 0, pinned: 0, archived: 0, trashed: 0, groups: [] } }) }] as never);
		expect(list.plan.has(ORCH)).toBe(false);
		expect(list.plan.has(OTHER)).toBe(true);
		list.applier.applyEvents([{ event: ev('plan_task_changed', { task: task({ id: 'a', revision: 5 }) }, '2') }] as never);
		expect(list.plan.has(ORCH)).toBe(false);
	});
});

describe('PlanModel: план, прочитанный по HTTP', () => {
	const A = '00000000-0000-4000-8000-000000000201';
	const B = '00000000-0000-4000-8000-000000000202';

	it('plan_loaded_tasks_count_even_without_the_snapshot', () => {
		const plan = new PlanModel();
		expect(plan.has(ORCH)).toBe(false);
		plan.loaded(ORCH, [task({ id: A, state: 'done', revision: 2 }), task({ id: B, number: 2, revision: 1 })]);
		expect(plan.has(ORCH)).toBe(true);
		expect(plan.of(ORCH).map((t) => t.id)).toEqual([A, B]);
		expect(plan.of(OTHER)).toEqual([]);
	});

	it('plan_live_overlays_loaded_only_with_a_newer_revision', () => {
		const plan = new PlanModel();
		plan.apply(task({ id: A, state: 'in_progress', revision: 3 }));
		plan.loaded(ORCH, [task({ id: A, state: 'waiting', revision: 2 }), task({ id: B, number: 2, state: 'done', revision: 5 })]);
		plan.replace([task({ id: A, state: 'in_progress', revision: 3 }), task({ id: B, number: 2, state: 'in_progress', revision: 4 })]);
		const states = Object.fromEntries(plan.of(ORCH).map((t) => [t.id, t.state]));
		expect(states).toEqual({ [A]: 'in_progress', [B]: 'done' });
		plan.apply(task({ id: B, number: 2, state: 'cancelled', revision: 6 }));
		expect(plan.of(ORCH).find((t) => t.id === B)?.state).toBe('cancelled');
	});

	it('plan_live_task_moved_to_another_orchestrator_leaves_the_loaded_plan', () => {
		const plan = new PlanModel();
		plan.loaded(ORCH, [task({ id: A, revision: 1 })]);
		plan.apply(task({ id: A, orchestrator_id: OTHER, revision: 2 }));
		expect(plan.of(ORCH)).toEqual([]);
		expect(plan.of(OTHER).map((t) => t.id)).toEqual([A]);
	});

	it('plan_dropped_orchestrator_forgets_the_loaded_plan_too', () => {
		const plan = new PlanModel();
		plan.loaded(ORCH, [task({ id: A })]);
		plan.dropOrchestrator(ORCH);
		expect(plan.of(ORCH)).toEqual([]);
		expect(plan.has(ORCH)).toBe(false);
		plan.loaded(OTHER, [task({ id: B, orchestrator_id: OTHER })]);
		plan.clear();
		expect(plan.has(OTHER)).toBe(false);
	});

	it('plan_loaded_does_not_bump_the_revision_counter', () => {
		const plan = new PlanModel();
		plan.loaded(ORCH, [task({ id: A })]);
		expect(plan.revisionOf(ORCH)).toBe(0);
	});
});
