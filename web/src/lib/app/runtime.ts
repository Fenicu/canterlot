import { me, eventsRange, snapshotConversations } from '$lib/api/client';
import { NetworkError } from '$lib/api/http';
import { DraftStore } from '$lib/drafts/store';
import { OpJournal } from '$lib/ops/journal';
import { opRequest } from '$lib/ops/request';
import { openDb, type Idb } from '$lib/storage/idb';
import { bindOwner } from '$lib/storage/owner';
import { ListStore, type ListSnapshot } from '$lib/stores/list.svelte';
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
	ownerId: string;
	db: Idb;
	journal: OpJournal;
	drafts: DraftStore;
	connection: ConnectionManager;
	list: ListStore;
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
		const who = await me();
		const ownerId = who.data.owner_id;
		await bindOwner({ owner_id: ownerId });
		const db = await openDb(ownerId);
		const journal = new OpJournal(
			db,
			async (op) => {
				const request = opRequest(op);
				const response = await deps.fetch(request.url, request.init);
				if (!response.ok) throw new NetworkError('send_failed');
				return response.json();
			},
			{
				now: deps.clock.now,
				schedule: deps.schedule,
				online: () => true,
				authExpired: () => false
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
			probeAuth: async () => 'ok',
			now: deps.clock.now,
			schedule: deps.schedule,
			rng: deps.rng ?? Math.random
		});
		const list = new ListStore({
			connection,
			api: {
				snapshot: async () => {
					const response = await snapshotConversations();
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
		const drafts = new DraftStore(ownerId, db);
		const runtime: Runtime = { ownerId, db, journal, drafts, connection, list };
		singleton = runtime;
		return runtime;
	}
}
