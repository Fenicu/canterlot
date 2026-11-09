import { describe, expect, it } from "vitest";
import type { OpJournal } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import { NetworkError } from "#lib/api/http.js";
import { intents } from "#lib/ops/intent.js";
import { sha256Hex } from "#lib/stores/blocks.js";
import { ChatStore, type ChatApi } from "#lib/stores/chat.svelte.js";
import type { HubEvent } from "#lib/sync/wire.js";

const CONV = "00000000-0000-4000-8000-0000000000ab";

function ev(kind: string, payload: unknown, seq: string): HubEvent {
  return {
    v: 1,
    id: `00000000-0000-4000-8000-${seq.padStart(12, "0")}`,
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: `conversation:${CONV}`,
    scope_seq: seq,
    scope_seq_from: seq,
    committed_at: "2026-10-28T12:00:00.000Z",
    committed_at_from: "2026-10-28T12:00:00.000Z",
    conversation_id: CONV,
    execution_id: "00000000-0000-4000-8000-0000000000e1",
    kind,
    payload,
  } as HubEvent;
}

function chat(): ChatStore {
  const api: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: async () => ({ items: [] }),
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
  };
  return new ChatStore(CONV, {
    api,
    ops: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: { subscribe: () => ({ release() {}, resync() {}, status: "idle", cursor: "0" }) } as unknown as ConnectionManager,
    blockIO: {
      fetchContent: async () => ({ content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 }),
      sha256Hex,
    },
    clock: { now: () => 0 },
    probe: {},
  });
}

async function lostKey(scope: string): Promise<string> {
  let used = "";
  await intents
    .run(scope, async (key) => {
      used = key;
      throw new NetworkError("timeout");
    })
    .catch(() => undefined);
  return used;
}

async function nextKey(scope: string): Promise<string> {
  let used = "";
  await intents.run(scope, async (key) => {
    used = key;
  });
  return used;
}

describe("исход команды в потоке событий", () => {
  it("смена состояния исполнения отпускает ключ потерянного «Стоп»", async () => {
    const store = chat();
    const lost = await lostKey(`stop:${CONV}:false`);
    store.applier.applyEvents([{ event: ev("execution_state_changed", { state: "stopping" }, "2"), overlap: false }]);
    expect(await nextKey(`stop:${CONV}:false`)).not.toBe(lost);
  });

  it("начало сжатия отпускает ключ потерянного «Сжать»", async () => {
    const store = chat();
    const lost = await lostKey(`compact:${CONV}`);
    store.applier.applyEvents([
      { event: ev("compaction_started", { id: "00000000-0000-4000-8000-0000000000c1", trigger: "manual" }, "3"), overlap: false },
    ]);
    expect(await nextKey(`compact:${CONV}`)).not.toBe(lost);
  });

  it("событие чужой сессии ключ не отпускает", async () => {
    const other = "00000000-0000-4000-8000-0000000000ff";
    const store = chat();
    const lost = await lostKey(`stop:${other}:false`);
    store.applier.applyEvents([{ event: ev("execution_state_changed", { state: "stopping" }, "4"), overlap: false }]);
    expect(await nextKey(`stop:${other}:false`)).toBe(lost);
  });
});
