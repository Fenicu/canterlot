import { describe, expect, it } from 'vitest';
import { FakeHub, FakeHubApi, FakeClock, seedMachine } from '../../../testing/fake-hub';
import { MachinesStore } from '#lib/stores/machines.svelte.js';
import type { ConnectionManager } from '#lib/sync/connection.js';

function setup() {
	const api = new FakeHubApi(new FakeHub());
	const worker = seedMachine(api, { name: 'carrier', availability: 'available' });
	const runner = seedMachine(api, { name: 'runner-a', role: 'runner', parent_machine_id: worker.id, availability: 'online', observed_at: '2026-11-21T12:00:00.000Z' });
	const store = new MachinesStore({
		connection: {} as ConnectionManager,
		clock: new FakeClock(1),
		api: { snapshot: () => api.machinesSnapshot(), events: async () => ({ events: [], head: '0' }) as never }
	});
	store.applier.applySnapshot({ items: [worker, runner] }, '1');
	const apply = (machine: string, availability: string) =>
		store.applier.applyEvents([{ event: { kind: 'machine_availability_changed', payload: { machine_id: machine, availability }, committed_at: '2026-11-21T12:05:00.000Z' } }] as never);
	return { worker, runner, store, apply };
}

describe('machines store: availability events of a runner', () => {
	it('keeps_the_runner_words_when_the_worker_connects', () => {
		const { runner, store, apply } = setup();
		apply(runner.id, 'unavailable');
		expect(store.items.get(runner.id)!.availability).toBe('unknown');
		apply(runner.id, 'available');
		expect(store.items.get(runner.id)!.availability).toBe('online');
	});

	it('does_not_claim_offline_without_the_hub_saying_so', () => {
		const { runner, store, apply } = setup();
		apply(runner.id, 'degraded');
		expect(store.items.get(runner.id)!.availability).toBe('unknown');
	});

	it('leaves_an_ordinary_machine_with_its_own_values', () => {
		const { worker, store, apply } = setup();
		apply(worker.id, 'degraded');
		expect(store.items.get(worker.id)!.availability).toBe('degraded');
		apply(worker.id, 'unavailable');
		expect(store.items.get(worker.id)!.availability).toBe('unavailable');
	});
});
