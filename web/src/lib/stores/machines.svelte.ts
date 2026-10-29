import type { components } from '#lib/api/generated/openapi.js';
import { SvelteMap } from 'svelte/reactivity';
import type { ConnectionManager } from '#lib/sync/connection.js';
import type { RangeResult, ScopeApplier, ScopeStatus } from '#lib/sync/scope.js';
import type { HubEvent } from '#lib/sync/wire.js';

export type MachineMetrics = Pick<components['schemas']['MetricsView'], 'observed_at'> & Partial<components['schemas']['MetricsView']>;

export interface MachineRecord {
	id: string;
	name: string;
	availability: string;
	os: string | null;
	heartbeat_age_ms?: number | null;
	heartbeat_received_at?: number;
	availability_since?: string | null;
	sessions?: components['schemas']['SessionView'][];
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
				for (const item of items) this.items.set(item.id, { ...item, heartbeat_received_at: this.deps.clock.now() });
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
		return this.deps.clock.now() - observed > (this.deps.staleAfterMs ?? 90_000);
	}

	#apply(event: HubEvent): void {
		const kind = event.kind as string;
		if (kind === 'machine_availability_changed' && event.kind === 'machine_availability_changed') {
			const machine = this.items.get(event.payload.machine_id);
			if (machine) this.items.set(machine.id, { ...machine, availability: event.payload.availability });
			return;
		}
		if (kind === 'machine_report_updated' && event.kind === 'machine_report_updated') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			machine.worker_version = event.payload.worker_version;
			machine.capabilities = event.payload.capabilities;
			machine.limits_ok = event.payload.limits_ok;
			this.items.set(machine.id, { ...machine });
			return;
		}
		if (kind === 'machine_metrics' && event.kind === 'machine_metrics') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			machine.metrics = {
				...event.payload,
				disks: event.payload.disks.filter((disk): disk is components['schemas']['DiskView'] => 'label' in disk && typeof disk.label === 'string' && 'total_bytes' in disk && typeof disk.total_bytes === 'number' && 'free_bytes' in disk && typeof disk.free_bytes === 'number'),
				executions: [],
				observed_at: event.committed_at
			};
			machine.warnings = machine.warnings.filter(warning => warning !== 'metrics_stale');
			machine.heartbeat_age_ms = 0;
			machine.heartbeat_received_at = this.deps.clock.now();
			this.items.set(machine.id, { ...machine });
			return;
		}
		if (kind !== 'hidden') this.unknownKinds += 1;
	}
}
