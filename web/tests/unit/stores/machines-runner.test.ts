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
	const apply = (machine: string, availability: string, observedAt?: string) =>
		store.applier.applyEvents([{ event: { kind: 'machine_availability_changed', payload: { machine_id: machine, availability, ...(observedAt ? { observed_at: observedAt } : {}) }, committed_at: '2026-11-21T12:05:00.000Z' } }] as never);
	return { worker, runner, store, apply };
}

describe('machines store: availability events of a runner', () => {
	it('takes_the_hub_runner_words_as_they_are', () => {
		const { runner, store, apply } = setup();
		apply(runner.id, 'offline', '2026-11-21T12:04:30.000Z');
		expect(store.items.get(runner.id)!.availability).toBe('offline');
		apply(runner.id, 'online');
		expect(store.items.get(runner.id)!.availability).toBe('online');
		apply(runner.id, 'unknown');
		expect(store.items.get(runner.id)!.availability).toBe('unknown');
	});

	it('takes_the_age_of_the_check_from_the_event', () => {
		const { runner, store, apply } = setup();
		apply(runner.id, 'offline', '2026-11-21T12:04:30.000Z');
		expect(store.items.get(runner.id)!.observed_at).toBe('2026-11-21T12:04:30.000Z');
		apply(runner.id, 'offline', '2026-11-21T12:05:30.000Z');
		expect(store.items.get(runner.id)!.observed_at).toBe('2026-11-21T12:05:30.000Z');
		// Без свежей проверки хаб `observed_at` не присылает: прежняя метка возраста не доживает.
		apply(runner.id, 'unknown');
		expect(store.items.get(runner.id)!.observed_at).toBeNull();
	});

	it('does_not_keep_the_queue_of_a_runner_that_left', () => {
		const { runner, store, apply } = setup();
		store.items.set(runner.id, { ...store.items.get(runner.id)!, lease: { holders: [], queue: [] } as never });
		apply(runner.id, 'online');
		expect(store.items.get(runner.id)!.lease).not.toBeNull();
		apply(runner.id, 'offline', '2026-11-21T12:04:30.000Z');
		expect(store.items.get(runner.id)!.lease).toBeNull();
	});

	it('leaves_an_ordinary_machine_with_its_own_values', () => {
		const { worker, store, apply } = setup();
		apply(worker.id, 'degraded');
		expect(store.items.get(worker.id)!.availability).toBe('degraded');
		apply(worker.id, 'unavailable');
		expect(store.items.get(worker.id)!.availability).toBe('unavailable');
	});
});
