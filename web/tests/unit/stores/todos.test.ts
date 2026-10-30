import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import { ChatStore, type ChatApi } from "#lib/stores/chat.svelte.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { OpJournal } from "#lib/ops/journal.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import type { HubEvent } from "#lib/sync/wire.js";
import { TodosModel, type TodoList } from "#lib/stores/todos.svelte.js";
import { countEffectRuns } from "./effect-runs.svelte";

const CONV = "00000000-0000-4000-8000-0000000000aa";
const LANE = "00000000-0000-4000-8000-0000000000b1";

function list(revision: number, text: string, lane: string | null = null): TodoList {
  return {
    lane_block_id: lane,
    revision,
    items: [{ text, status: "in_progress" }],
    omitted: 0,
  };
}

function ev(kind: string, payload: unknown, seq: string): HubEvent {
  return {
    v: 1,
    id: "00000000-0000-4000-8000-0000000000ee",
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: `conversation:${CONV}`,
    scope_seq: seq,
    scope_seq_from: seq,
    committed_at: "2026-10-28T12:00:00.000Z",
    committed_at_from: "2026-10-28T12:00:00.000Z",
    conversation_id: CONV,
    execution_id: null,
    kind,
    payload,
  } as HubEvent;
}

function chatOf(data: Record<string, unknown> = {}): ChatStore {
  const api: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data }),
    history: async () => ({ items: [] }),
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
  };
  return new ChatStore(CONV, {
    api,
    ops: {
      ops: new Map(),
      settle() {},
      onOrphanRefused: () => () => undefined,
    } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: {
      subscribe: () => ({ release() {}, status: "idle", cursor: "0" }),
    } as unknown as ConnectionManager,
    blockIO: {
      fetchContent: async () => ({
        content: "",
        offset: 0,
        rev: 0,
        state: "streaming",
        total_bytes: 0,
      }),
      sha256Hex: async () => "",
    },
    clock: { now: () => 0 },
    probe: {},
  });
}

describe("stores/todos", () => {
  it("newer_revision_replaces_the_list_and_stale_one_is_ignored", () => {
    const model = new TodosModel();
    model.apply(list(2, "новый"));
    model.apply(list(1, "старый"));
    model.apply(list(2, "дубль"));
    expect(model.lane(null)?.items[0]?.text).toBe("новый");
    model.apply(list(3, "свежий"));
    expect(model.lane(null)?.revision).toBe(3);
  });

  it("lanes_do_not_mix", () => {
    const model = new TodosModel();
    model.apply(list(1, "главный"));
    model.apply(list(1, "субагент", LANE));
    expect(model.lane(null)?.items[0]?.text).toBe("главный");
    expect(model.lane(LANE)?.items[0]?.text).toBe("субагент");
    expect(model.lane("other")).toBeUndefined();
  });

  it("snapshot_never_rolls_back_a_list_that_arrived_before_it", () => {
    const model = new TodosModel();
    model.apply(list(5, "пришло событием"));
    model.ingestSnapshot([list(4, "снимок старее"), list(1, "субагент", LANE)]);
    expect(model.lane(null)?.items[0]?.text).toBe("пришло событием");
    expect(model.lane(LANE)?.revision).toBe(1);
  });
});

describe("chat store: todo_updated", () => {
  it("event_updates_the_lane_in_place", () => {
    const chat = chatOf();
    chat.applier.applyEvents([
      { event: ev("todo_updated", list(1, "план"), "1"), overlap: false },
    ]);
    expect(chat.todos.lane(null)?.items[0]?.text).toBe("план");
    expect(chat.unknownKinds).toBe(0);
  });

  it("list_stays_live_while_the_owner_reads_history", () => {
    const chat = chatOf();
    chat.timeline.tailDetached = true;
    chat.applier.applyEvents([
      { event: ev("todo_updated", list(1, "план"), "1"), overlap: false },
    ]);
    expect(chat.todos.lane(null)?.revision).toBe(1);
  });

  it("deltas_do_no_work_for_the_list", () => {
    const chat = chatOf();
    chat.applier.applyEvents([
      { event: ev("todo_updated", list(1, "план"), "1"), overlap: false },
    ]);
    const effect = countEffectRuns(() => void chat.todos.lane(null));
    const before = effect.runs();
    chat.applier.applyEvents([
      {
        event: ev(
          "block_delta",
          { block_id: LANE, rev: 1, append: "токен" },
          "2",
        ),
        overlap: false,
      },
    ]);
    expect(effect.runs()).toBe(before);
    effect.stop();
  });
});
