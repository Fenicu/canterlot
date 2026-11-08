import { describe, expect, it } from 'vitest';
import { FakeHub, FakeHubApi, FakeClock, machineWatchSeed, seedMachine } from '../../../testing/fake-hub';
import { MachinesStore } from '#lib/stores/machines.svelte.js';
import type { ConnectionManager } from '#lib/sync/connection.js';

function setup() {
	const api = new FakeHubApi(new FakeHub());
	const machine = seedMachine(api, { warnings: ['disk_usage_high'] });
	const store = new MachinesStore({
		connection: {} as ConnectionManager,
		clock: new FakeClock(1),
		api: { snapshot: () => api.machinesSnapshot(), events: async () => ({ events: [], head: '0' }) as never }
	});
	store.applier.applySnapshot({ items: [machine] }, '1');
	const apply = (payload: unknown) => store.applier.applyEvents([{ event: { kind: 'machine_watch_changed', payload } }] as never);
	return { api, machine, store, apply };
}

describe('machines store: machine_watch_changed', () => {
	it('replaces_the_watch_report_and_sets_its_warnings', () => {
		const { api, machine, store, apply } = setup();
		const seed = machineWatchSeed(api);
		apply({ machine_id: machine.id, watch: seed.watch, memory_low: true });
		const record = store.items.get(machine.id)!;
		expect(record.watch?.hotspots).toHaveLength(4);
		expect(record.watch?.unleased_count).toBe(2);
		expect(record.warnings).toEqual(['disk_usage_high', 'memory_low', 'processes_leaked']);
		expect(store.unknownKinds).toBe(0);
	});

	it('clears_warnings_when_the_next_report_is_clean', () => {
		const { api, machine, store, apply } = setup();
		const seed = machineWatchSeed(api);
		apply({ machine_id: machine.id, watch: seed.watch, memory_low: true });
		apply({ machine_id: machine.id, watch: { ...seed.watch, leaked: [] }, memory_low: false });
		expect(store.items.get(machine.id)!.warnings).toEqual(['disk_usage_high']);
		expect(store.items.get(machine.id)!.watch?.leaked).toEqual([]);
		apply({ machine_id: machine.id, watch: null, memory_low: true });
		expect(store.items.get(machine.id)!.watch).toBeNull();
		expect(store.items.get(machine.id)!.warnings).toEqual(['disk_usage_high', 'memory_low']);
	});

	it('ignores_unknown_machines', () => {
		const { api, store, apply } = setup();
		apply({ machine_id: api.id(), watch: null, memory_low: true });
		expect([...store.items.values()].every(item => !item.warnings.includes('memory_low'))).toBe(true);
	});
});
