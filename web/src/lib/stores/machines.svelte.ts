import type { components } from '#lib/api/generated/openapi.js';
import { SvelteMap } from 'svelte/reactivity';
import type { ConnectionManager } from '#lib/sync/connection.js';
import type { RangeResult, ScopeApplier, ScopeStatus } from '#lib/sync/scope.js';
import type { HubEvent } from '#lib/sync/wire.js';
import { toUpdateItem, type UpdateItem } from '#lib/updates/text.js';

export type MachineMetrics = Pick<components['schemas']['MetricsView'], 'observed_at'> & Partial<components['schemas']['MetricsView']>;

export type MachineWatch = components['schemas']['WatchView'];

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
	harnesses: { id: string; title: string; provider: string; models: { id: string; title: string; efforts: string[]; images?: boolean | null; context_window?: number | null }[]; permission_modes: string[]; settings_midsession?: boolean; settings_midsession_effort?: boolean; catalog?: { name: string; description: string; kind: string; argument_hint: string }[] }[];
	/** Конец входа провайдера на этой машине: по строке на провайдера с известной датой. */
	credentials?: { provider: string; expires_at: string; observed_at: string }[];
	worker_version?: string | null;
	capabilities?: string[];
	limits_ok?: boolean;
	/** Принимает ли машина делегатов и команды других машин; включает владелец. */
	accepts_remote_work?: boolean;
	allowed_roots?: string[];
	/** `worker` (обычная машина) или `runner`; у снимка старого хаба поля нет. */
	role?: string;
	/** Несущая машина раннера. */
	parent_machine_id?: string | null;
	/** Ранг размещения раннера: больше — предпочтительнее. */
	rank?: number;
	labels?: string[];
	/** Машина помечена несущей раннеров. */
	is_carrier?: boolean;
	/** Раннер: когда несущая последний раз проверила его достижимость. */
	observed_at?: string | null;
	home_dir?: string | null;
	/** Отчёт сторожа машины: крупные кэши сборки, процессы без сессии, сборки мимо очереди. */
	watch?: MachineWatch | null;
	/** Очередь тяжёлых задач машины; `null` без свежего состояния. */
	lease?: components['schemas']['LeaseStateView'] | null;
}

export type JobRecord = components['schemas']['JobView'];

const JOB_RANK: Record<string, number> = { waiting_runner: 0, staging: 1, queued: 2, dispatched: 3, waiting_lease: 4, running: 5, collecting: 6 };

/** Состояние задачи ведёт только вперёд: опоздавшая копия, список или событие, прежнее не затирает. */
function jobMayReplace(old: JobRecord | undefined, next: JobRecord): boolean {
	if (!old) return true;
	const was = JOB_RANK[old.state];
	if (was === undefined) return next.state === old.state;
	const now = JOB_RANK[next.state];
	return now === undefined || now >= was;
}

export class MachinesStore {
	readonly items = new SvelteMap<string, MachineRecord>();
	/** Задачи на месте: события машин и списки, которые читали экраны. */
	readonly jobs = new SvelteMap<string, JobRecord>();
	/** Растёт с каждой перезагрузкой снимка: события могли пропасть, экраны читают задачи заново. */
	jobsEpoch = $state(0);
	unknownKinds = $state(0);
	/** Живые записи ленты обновлений приходят в области машин, без опроса ленты. */
	onUpdate: ((item: UpdateItem) => void) | null = null;
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
				this.jobsEpoch += 1;
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

	/** Принять задачи из ответа хаба; то, что событие уже довело дальше, остаётся как есть. */
	mergeJobs(items: readonly JobRecord[]): void {
		for (const job of items) if (jobMayReplace(this.jobs.get(job.id), job)) this.jobs.set(job.id, job);
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
		if (kind === 'machine_update' && event.kind === 'machine_update') {
			this.onUpdate?.(toUpdateItem(event.payload as unknown as Record<string, unknown>));
			return;
		}
		if (kind === 'machine_availability_changed' && event.kind === 'machine_availability_changed') {
			const machine = this.items.get(event.payload.machine_id);
			// Пропавшая машина не подтверждает очередь: хаб снимает её тем же решением.
			if (!machine) return;
			// Раннеру хаб присылает свои слова `online|offline|unknown` и возраст проверки несущей `observed_at`;
			// без свежей проверки поля нет, и прежний возраст не доживает. Обычная машина остаётся со старыми значениями.
			if (machine.role === 'runner') {
				const word = event.payload.availability;
				const availability = word === 'online' || word === 'offline' ? word : 'unknown';
				this.items.set(machine.id, { ...machine, availability, observed_at: event.payload.observed_at ?? null, availability_since: event.committed_at, ...(availability === 'online' ? {} : { lease: null }) });
				return;
			}
			this.items.set(machine.id, { ...machine, availability: event.payload.availability, availability_since: event.committed_at, ...(event.payload.availability === 'unavailable' ? { lease: null } : {}) });
			return;
		}
		if (kind === 'machine_report_updated' && event.kind === 'machine_report_updated') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			machine.worker_version = event.payload.worker_version;
			machine.capabilities = event.payload.capabilities;
			machine.limits_ok = event.payload.limits_ok;
			machine.home_dir = event.payload.home_dir ?? null;
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
		if (kind === 'machine_lease_changed' && event.kind === 'machine_lease_changed') {
			const machine = this.items.get(event.payload.machine_id);
			if (machine) this.items.set(machine.id, { ...machine, lease: event.payload.lease ?? null });
			return;
		}
		if (kind === 'machine_journal_stops_changed' && event.kind === 'machine_journal_stops_changed') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			const kept = machine.warnings.filter(warning => !warning.endsWith('_journal_stopped'));
			this.items.set(machine.id, { ...machine, warnings: [...kept, ...event.payload.stopped.map(kind => `${kind}_journal_stopped`)] });
			return;
		}
		if (kind === 'machine_watch_changed' && event.kind === 'machine_watch_changed') {
			const machine = this.items.get(event.payload.machine_id);
			if (!machine) return;
			// Событие несёт полное состояние: прежний отчёт и оба его предупреждения заменяются целиком.
			const watch = event.payload.watch;
			const kept = machine.warnings.filter(warning => warning !== 'memory_low' && warning !== 'processes_leaked');
			const warnings = [...kept, ...(event.payload.memory_low ? ['memory_low'] : []), ...((watch?.leaked.length ?? 0) > 0 ? ['processes_leaked'] : [])];
			this.items.set(machine.id, { ...machine, watch, warnings });
			return;
		}
		if (kind === 'job_changed' && event.kind === 'job_changed') {
			this.mergeJobs([event.payload.job]);
			return;
		}
		if (kind !== 'hidden') this.unknownKinds += 1;
	}
}
