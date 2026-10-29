import { SvelteMap } from 'svelte/reactivity';
import type { ConnectionManager } from '$lib/sync/connection';
import type { RangeResult, ScopeApplier, ScopeStatus } from '$lib/sync/scope';
import type { HubEvent } from '$lib/sync/wire';

export interface MachineMetrics {
	observed_at: string;
	cpu_permille?: number;
	mem_available_bytes?: number;
	machine_time_ms?: number;
}

export interface MachineRecord {
	id: string;
	name: string;
	availability: string;
	os: string | null;
	warnings: string[];
	metrics: MachineMetrics | null;
	harnesses: { id: string; title: string; provider: string; models: { id: string; title: string; efforts: string[] }[]; permission_modes: string[] }[];
	profiles: { name: string; title: string; description: string; harnesses: string[] }[];
	worker_version?: string | null;
	capabilities?: string[];
	limits_ok?: boolean;
	allowed_roots?: string[];
}

export class MachinesStore {
	readonly items = new SvelteMap<string, MachineRecord>();
	unknownKinds = $state(0);
	#sub: { release(): void; readonly status: ScopeStatus } | null = null;

	constructor(
		private deps: {
			api: {
				snapshot(): Promise<{ cursor: string; data: { items: MachineRecord[] } }>;
				events(after: string, limit: number): Promise<RangeResult>;
			};
			connection: ConnectionManager;
			clock: { now(): number };
			staleAfterMs?: number;
		}
	) {}

	get status(): ScopeStatus {
		return this.#sub?.status ?? 'idle';
	}

	get applier(): ScopeApplier {
		return {
			loadSnapshot: async () => {
				const snap = await this.deps.api.snapshot();
				return { cursor: snap.cursor, data: snap.data };
			},
			applySnapshot: (data) => {
				const items = (data as { items?: MachineRecord[] }).items ?? [];
				this.items.clear();
				for (const item of items) this.items.set(item.id, item);
			},
			applyEvents: (batch) => {
				for (const item of batch) this.#apply(item.event);
			},
			fetchRange: (after, limit) => this.deps.api.events(after, limit)
		};
	}

	open(): void {
		if (!this.#sub) this.#sub = this.deps.connection.subscribe('machines', this.applier);
	}

	close(): void {
		this.#sub?.release();
		this.#sub = null;
	}

	catalog(): MachineRecord['harnesses'] {
		const out: MachineRecord['harnesses'] = [];
		for (const machine of this.items.values()) out.push(...machine.harnesses);
		return out;
	}

	metricsStale(id: string): boolean {
		const machine = this.items.get(id);
		if (!machine) return false;
		if (machine.warnings.includes('metrics_stale')) return true;
		if (!machine.metrics?.observed_at) return false;
		const observed = Date.parse(machine.metrics.observed_at);
		if (Number.isNaN(observed)) return true;
		return this.deps.clock.now() - observed > (this.deps.staleAfterMs ?? 60_000);
	}

	#apply(event: HubEvent): void {
		const kind = event.kind as string;
		if (kind === 'machine_availability_changed' && event.kind === 'machine_availability_changed') {
			const machine = this.items.get(event.payload.machine_id);
			if (machine) machine.availability = event.payload.availability;
			return;
		}
		if (kind === 'machine_report_updated' && event.kind === 'machine_report_updated') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			machine.worker_version = event.payload.worker_version;
			machine.capabilities = event.payload.capabilities;
			machine.limits_ok = event.payload.limits_ok;
			return;
		}
		if (kind === 'machine_metrics' && event.kind === 'machine_metrics') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			machine.metrics = {
				observed_at: event.committed_at,
				cpu_permille: event.payload.cpu_permille,
				mem_available_bytes: event.payload.mem_available_bytes,
				machine_time_ms: event.payload.machine_time_ms
			};
			return;
		}
		if (kind !== 'hidden') this.unknownKinds += 1;
	}
}
