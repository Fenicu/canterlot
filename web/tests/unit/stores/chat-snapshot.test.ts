import { describe, expect, it } from "vitest";
import type { OpJournal } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import { sha256Hex } from "#lib/stores/blocks.js";
import { ChatStore, type ChatApi, type HistoryItem, type SnapshotData } from "#lib/stores/chat.svelte.js";
import type { HubEvent } from "#lib/sync/wire.js";

const CONV = "00000000-0000-4000-8000-0000000000aa";

export function ev(kind: string, payload: unknown, seq = "1", execution: string | null = null): HubEvent {
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
    execution_id: execution,
    kind,
    payload,
  } as HubEvent;
}

export function chatOf(api: Partial<ChatApi> = {}, resync: () => void = () => undefined): ChatStore {
  const full: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: async () => ({ items: [] }),
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
    ...api,
  };
  return new ChatStore(CONV, {
    api: full,
    ops: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: { subscribe: () => ({ release() {}, resync, status: "idle", cursor: "0" }) } as unknown as ConnectionManager,
    blockIO: {
      fetchContent: async () => ({ content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 }),
      sha256Hex,
    },
    clock: { now: () => 0 },
    probe: {},
  });
}

function snapshot(chat: ChatStore, data: SnapshotData): void {
  chat.applier.applySnapshot(data, "0");
}

function apply(chat: ChatStore, ...events: HubEvent[]): void {
  chat.applier.applyEvents(events.map((event) => ({ event, overlap: false })));
}

const permission = (id: string) => ({
  id,
  conversation_id: CONV,
  kind: "permission",
  state: "open",
  request_payload: { command: "ls" },
});

describe("снимок тёплого чата", () => {
  it("снимок без живого исполнения снимает старое исполнение, фазу и прерывание", () => {
    const chat = chatOf();
    snapshot(chat, { detail: { execution: { execution_id: "e1", state: "running" } } });
    apply(
      chat,
      ev("phase_changed", { phase: "context_maintenance" }, "2"),
      ev("interrupt_state_changed", { status: "requested" }, "3"),
    );
    snapshot(chat, { detail: { execution: null } });
    expect(chat.execution).toBeNull();
    expect(chat.phase).toBeNull();
    expect(chat.interrupt).toBeNull();
  });

});
