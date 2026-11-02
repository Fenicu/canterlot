import {
  me,
  eventsRange,
  snapshotConversations,
  snapshotMachines,
} from "#lib/api/client.js";
import { AuthRequired, NetworkError, authProbeResult, session } from "#lib/api/http.js";
import { returnRefused } from "#lib/composer/returned.js";
import { DraftStore } from "#lib/drafts/store.js";
import { OpJournal } from "#lib/ops/journal.js";
import { sendOp } from "#lib/ops/request.js";
import { openDb, type Idb } from "#lib/storage/idb.js";
import { bindOwner, onOwnerRetired } from "#lib/storage/owner.js";
import { staleness } from "#lib/pwa/sync-state.js";
import { sweepShares } from "#lib/pwa/share.js";
import { ListStore, type ListSnapshot } from "#lib/stores/list.svelte.js";
import {
  MachinesStore,
  type MachineRecord,
} from "#lib/stores/machines.svelte.js";
import { browserLifecycle } from "#lib/sync/lifecycle.js";
import type { RangeResult } from "#lib/sync/scope.js";
import { ConnectionManager } from "#lib/sync/connection.js";
import { BrowserSocket } from "#lib/sync/transport.js";

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
  if (!singleton) throw new Error("runtime_missing");
  return singleton;
}

export async function createRuntime(deps: RuntimeDeps): Promise<Runtime> {
  globalThis.fetch = deps.fetch;
  globalThis.WebSocket = deps.WebSocket;
  {
    let ownerId: string;
    try {
      if (!navigator.onLine) throw new NetworkError("offline");
      const who = await me();
      ownerId = who.data.owner_id;
      await bindOwner({ owner_id: ownerId });
    } catch (error) {
      // Истёкшая сессия не должна оставлять приложение без runtime: диалог входа
      // откроется после старта, а сохранённый владелец даст показать локальные данные.
      if (!(error instanceof NetworkError) && !(error instanceof AuthRequired)) throw error;
      const saved = await (
        await openDb("__system__")
      ).get<string>("kv", "current_owner");
      if (!saved) throw error;
      ownerId = saved;
    }
    const db = await openDb(ownerId);
    await sweepShares(db, deps.clock.now());
    staleness.lastSyncAt = (await db.get<number>("kv", "last_sync_at")) ?? null;
    const journal = new OpJournal(
      db,
      (op) => sendOp(deps.fetch, op),
      {
        now: deps.clock.now,
        schedule: deps.schedule,
        online: () => navigator.onLine,
        authExpired: () => session.state === "expired",
      },
    );
    const drafts = new DraftStore(ownerId, db);
    journal.onOrphanRefused((op) => void returnRefused(op, drafts));
    await journal.recover();
    const connection = new ConnectionManager({
      transport: new BrowserSocket(() => {
        const origin =
          typeof location === "undefined"
            ? "http://localhost"
            : location.origin;
        return `${origin.replace(/^http/, "ws")}/api/v1/ws`;
      }),
      lifecycle: browserLifecycle({
        window,
        document,
        wall: deps.clock.now,
        monotonic: deps.clock.now,
        schedule: deps.schedule,
      }),
      probeAuth: async () => {
        try {
          const who = await me();
          if (who.data.owner_id !== ownerId) {
            await bindOwner(who.data);
            location.reload();
            return "expired";
          }
          return "ok";
        } catch (error) {
          return authProbeResult(error);
        }
      },
      now: deps.clock.now,
      schedule: deps.schedule,
      rng: deps.rng ?? Math.random,
    });
    const list = new ListStore({
      connection,
      api: {
        snapshot: async () => {
          const response = await snapshotConversations();
          const syncedAt = deps.clock.now();
          staleness.synced(syncedAt);
          await db.put("kv", "last_sync_at", syncedAt);
          return {
            cursor: response.data.cursor,
            data: response.data.data as unknown as ListSnapshot,
          };
        },
        events: async (after, limit): Promise<RangeResult> => {
          const response = await eventsRange({
            scope: "conversations",
            after,
            limit,
          });
          const body = response.data as {
            events?: RangeResult extends { events: infer E } ? E : never;
            head?: string;
            resync_required?: unknown;
          };
          if (body && typeof body === "object" && "resync_required" in body)
            return { resync: true };
          return { events: body.events ?? [], head: body.head ?? after };
        },
      },
    });
    list.onDeleted((id) => {
      journal.discardConversation(id);
      void drafts.clear(id);
    });
    list.open();
    const machines = new MachinesStore({
      connection,
      clock: deps.clock,
      api: {
        snapshot: async () => {
          const response = await snapshotMachines();
          return {
            cursor: response.data.cursor,
            data: response.data.data as unknown as { items: MachineRecord[] },
          };
        },
        events: async (after, limit): Promise<RangeResult> => {
          const response = await eventsRange({
            scope: "machines",
            after,
            limit,
          });
          const body = response.data as {
            events?: RangeResult extends { events: infer E } ? E : never;
            head?: string;
            resync_required?: unknown;
          };
          if (body && typeof body === "object" && "resync_required" in body)
            return { resync: true };
          return { events: body.events ?? [], head: body.head ?? after };
        },
      },
    });
    machines.open();
    let disposed = false;
    const runtime: Runtime = {
      ownerId,
      db,
      journal,
      drafts,
      connection,
      list,
      machines,
      dispose() {
        if (disposed) return;
        disposed = true;
        connection.dispose();
        journal.retire();
        drafts.retire();
        list.close();
        list.items.clear();
        list.deleted.clear();
        list.groups = [];
        list.limits = [];
        list.presets = [];
        list.counts = { all: 0, pinned: 0, archived: 0, groups: [] };
        machines.close();
        machines.items.clear();
        if (singleton === runtime) singleton = null;
        release();
      },
    };
    const release = onOwnerRetired(() => runtime.dispose());
    singleton = runtime;
    return runtime;
  }
}
