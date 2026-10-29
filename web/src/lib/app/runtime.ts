import { me, eventsRange, snapshotConversations, snapshotMachines } from '$lib/api/client';
import { ApiError, NetworkError, session, type ErrorCode, type ErrorDetails, type ErrorScope, type Retry } from '$lib/api/http';
import { DraftStore } from '$lib/drafts/store';
import { OpJournal } from '$lib/ops/journal';
import { opRequest } from '$lib/ops/request';
import { openDb, type Idb } from '$lib/storage/idb';
import { bindOwner, onOwnerRetired } from '$lib/storage/owner';
import { staleness } from '$lib/pwa/offline';
import { sweepShares } from '$lib/pwa/share';
import { ListStore, type ListSnapshot } from '$lib/stores/list.svelte';
import { MachinesStore, type MachineRecord } from '$lib/stores/machines.svelte';
import { browserLifecycle } from '$lib/sync/lifecycle';
import type { RangeResult } from '$lib/sync/scope';
import { ConnectionManager } from '$lib/sync/connection';
import { BrowserSocket } from '$lib/sync/transport';

export interface RuntimeDeps {
	fetch: typeof fetch;
	WebSocket: typeof WebSocket;
	indexedDB: IDBFactory;
	clock: { now(): number };
	schedule: (fn: () => void, ms: number) => () => void;
	rng?: () => number;
}

export interface Runtime {
	dispose(): void;
	ownerId: string;
	db: Idb;
	journal: OpJournal;
	drafts: DraftStore;
	connection: ConnectionManager;
	list: ListStore;
	machines: MachinesStore;
}

let singleton: Runtime | null = null;

export function getRuntime(): Runtime {
	if (!singleton) throw new Error('runtime_missing');
	return singleton;
}

export async function createRuntime(deps: RuntimeDeps): Promise<Runtime> {
	globalThis.fetch = deps.fetch;
	globalThis.WebSocket = deps.WebSocket;
	{
		let ownerId: string;
		try {
			if (!navigator.onLine) throw new NetworkError('offline');
			const who = await me();
			ownerId = who.data.owner_id;
			await bindOwner({ owner_id: ownerId });
		} catch (error) {
			if (!(error instanceof NetworkError)) throw error;
			const saved = await (await openDb('__system__')).get<string>('kv', 'current_owner');
			if (!saved) throw error;
			ownerId = saved;
		}
		const db = await openDb(ownerId);
		await sweepShares(db, deps.clock.now());
		staleness.lastSyncAt = await db.get<number>('kv', 'last_sync_at') ?? null;
		const journal = new OpJournal(
			db,
			async (op) => {
				const request = opRequest(op);
				const response = await deps.fetch(request.url, request.init);
				const body = await response.json().catch(() => null);
				if (!response.ok) throw httpFailure(response, body);
				return body;
			},
			{
				now: deps.clock.now,
				schedule: deps.schedule,
				online: () => navigator.onLine,
				authExpired: () => session.state === 'expired'
			}
		);
		await journal.recover();
		const connection = new ConnectionManager({
			transport: new BrowserSocket(() => {
				const origin = typeof location === 'undefined' ? 'http://localhost' : location.origin;
				return `${origin.replace(/^http/, 'ws')}/api/v1/ws`;
			}),
			lifecycle: browserLifecycle({
				window,
				document,
				wall: deps.clock.now,
				monotonic: deps.clock.now,
				schedule: deps.schedule
			}),
			probeAuth: async () => {
				try {
					const who = await me();
					if (who.data.owner_id !== ownerId) { await bindOwner(who.data); location.reload(); return 'expired'; }
					return 'ok';
				} catch { return session.state === 'expired' ? 'expired' : 'down'; }
			},
			now: deps.clock.now,
			schedule: deps.schedule,
			rng: deps.rng ?? Math.random
		});
		const list = new ListStore({
			connection,
			api: {
				snapshot: async () => {
					const response = await snapshotConversations();
					const syncedAt = deps.clock.now();
					staleness.synced(syncedAt);
					await db.put('kv', 'last_sync_at', syncedAt);
					return { cursor: response.data.cursor, data: response.data.data as unknown as ListSnapshot };
				},
				events: async (after, limit): Promise<RangeResult> => {
					const response = await eventsRange({ scope: 'conversations', after, limit });
					const body = response.data as { events?: RangeResult extends { events: infer E } ? E : never; head?: string; resync_required?: unknown };
					if (body && typeof body === 'object' && 'resync_required' in body) return { resync: true };
					return { events: body.events ?? [], head: body.head ?? after };
				}
			}
		});
		list.open();
		const machines = new MachinesStore({
			connection,
			clock: deps.clock,
			api: {
				snapshot: async () => {
					const response = await snapshotMachines();
					return { cursor: response.data.cursor, data: response.data.data as unknown as { items: MachineRecord[] } };
				},
				events: async (after, limit): Promise<RangeResult> => {
					const response = await eventsRange({ scope: 'machines', after, limit });
					const body = response.data as { events?: RangeResult extends { events: infer E } ? E : never; head?: string; resync_required?: unknown };
					if (body && typeof body === 'object' && 'resync_required' in body) return { resync: true };
					return { events: body.events ?? [], head: body.head ?? after };
				}
			}
		});
		machines.open();
		const drafts = new DraftStore(ownerId, db);
		let disposed = false;
		const runtime: Runtime = { ownerId, db, journal, drafts, connection, list, machines, dispose() {
			if (disposed) return;
			disposed = true;
			connection.dispose(); journal.retire(); drafts.retire();
			list.close(); list.items.clear(); list.groups = []; list.limits = []; list.counts = { all: 0, pinned: 0, archived: 0, groups: [] };
			machines.close(); machines.items.clear();
			if (singleton === runtime) singleton = null;
			release();
		} };
		const release = onOwnerRetired(() => runtime.dispose());
		singleton = runtime;
		return runtime;
	}
}

function httpFailure(response: Response, body: unknown): Error {
	const error = (body as { error?: { code?: string; message?: string; scope?: string; retry?: string; correlation_id?: string; details?: ErrorDetails } } | null)?.error;
	if (error?.code && error.message && error.scope && error.retry && error.correlation_id) {
		return new ApiError(
			response.status,
			error.code as ErrorCode,
			error.message,
			error.scope as ErrorScope,
			error.retry as Retry,
			error.correlation_id,
			error.details
		);
	}
	return new NetworkError('send_failed');
}
