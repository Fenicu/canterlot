import { describe, expect, it, vi } from "vitest";
import type { OpJournal } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import { sha256Hex, type BlockPage } from "#lib/stores/blocks.js";
import { ChatStore, type HistoryItem } from "#lib/stores/chat.svelte.js";
import type { HubEvent } from "#lib/sync/wire.js";

const CONV = "00000000-0000-4000-8000-0000000000aa";
const ID = "00000000-0000-4000-8000-0000000000b7";
/** Хаб отдаёт в истории только начало вывода инструмента (TOOL_PREVIEW_BYTES). */
const PREVIEW = 4096;

function chatWith(fetchContent: (blockId: string, offset: number) => Promise<BlockPage>): ChatStore {
  return new ChatStore(CONV, {
    api: {
      conversationSnapshot: async () => ({ cursor: "0", data: {} }),
      history: async () => ({ items: [] }),
      events: async () => ({ events: [], head: "0" }),
      markRead: async () => undefined,
    },
    ops: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: { subscribe: () => ({ release() {}, resync() {}, status: "idle", cursor: "0" }) } as unknown as ConnectionManager,
    blockIO: { fetchContent, sha256Hex },
    clock: { now: () => 0 },
    probe: {},
  });
}

function ev(kind: string, payload: unknown, seq: string): HubEvent {
  return {
    v: 1,
    id: `00000000-0000-4000-8000-${seq.padStart(12, "0")}`,
    correlation_id: ID,
    scope: `conversation:${CONV}`,
    scope_seq: seq,
    scope_seq_from: seq,
    committed_at: "2026-10-28T12:00:01.000Z",
    committed_at_from: "2026-10-28T12:00:01.000Z",
    conversation_id: CONV,
    execution_id: null,
    kind,
    payload,
  } as unknown as HubEvent;
}

function apply(chat: ChatStore, ...events: HubEvent[]): void {
  chat.applier.applyEvents(events.map((event) => ({ event, overlap: false })));
}

/** Идущий вывод длиной `total` байт на ревизии `rev`, как его показывает история. */
function running(total: number, rev: number): HistoryItem {
  return {
    kind: "block",
    id: ID,
    block_kind: "tool_result",
    content: "A".repeat(Math.min(total, PREVIEW)),
    content_bytes: total,
    content_truncated: total > PREVIEW,
    created_seq: "10",
    created_at: "2026-10-28T12:00:00.000Z",
    rev,
    state: "streaming",
  };
}

describe("идущий вывод инструмента после снимка", () => {
  it("начало из истории не склеивается с дельтой: тело догружается целиком", async () => {
    const full = "A".repeat(PREVIEW) + "B".repeat(8000);
    const offsets: number[] = [];
    const chat = chatWith(async (_id, offset) => {
      offsets.push(offset);
      return { content: full, offset: 0, rev: 50, state: "streaming", total_bytes: full.length };
    });
    chat.applier.applySnapshot({ history: { items: [running(full.length, 50)] } }, "100");
    // Дельта пришла раньше, чем догрузилось тело: она ждёт своей очереди.
    apply(chat, ev("block_delta", { block_id: ID, base_rev: 50, rev: 51, append: "TAIL" }, "101"));

    const block = chat.blocks.get(ID)!;
    await vi.waitFor(() => expect(block.state).toBe("streaming"));
    expect(offsets).toEqual([0]);
    expect(block.body).toBe(full + "TAIL");
    expect(block.bytes).toBe(full.length + 4);
    expect(block.rev).toBe(51);
    expect(block.truncated).toBe(false);
  });

  it("живое тело той же ревизии начало из снимка не заменяет", () => {
    let fetches = 0;
    const chat = chatWith(async () => {
      fetches += 1;
      return { content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 };
    });
    const live = "A".repeat(PREVIEW) + "B".repeat(8000);
    apply(
      chat,
      ev("block_started", { block_id: ID, rev: 0, kind: "tool_result", input_command_id: null, parent_block_id: null, agent: "a", model: "m", tool_name: null, summary: null, approved_by: null }, "10"),
      ev("block_delta", { block_id: ID, base_rev: 0, rev: 1, append: live }, "11"),
    );
    // Возврат в тёплый чат: снимок несёт тот же блок, но только его начало.
    chat.applier.applySnapshot({ history: { items: [running(live.length, 1)] } }, "11");
    apply(chat, ev("block_delta", { block_id: ID, base_rev: 1, rev: 2, append: "TAIL" }, "12"));

    const block = chat.blocks.get(ID)!;
    expect(block.state).toBe("streaming");
    expect(block.body).toBe(live + "TAIL");
    expect(block.bytes).toBe(live.length + 4);
    expect(block.truncated).toBe(false);
    expect(fetches).toBe(0);
  });

  it("короткий идущий вывод приходит в истории целиком и не догружается", () => {
    let fetches = 0;
    const chat = chatWith(async () => {
      fetches += 1;
      return { content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 };
    });
    chat.applier.applySnapshot({ history: { items: [running(100, 5)] } }, "100");
    apply(chat, ev("block_delta", { block_id: ID, base_rev: 5, rev: 6, append: "TAIL" }, "101"));
    const block = chat.blocks.get(ID)!;
    expect(block.body).toBe("A".repeat(100) + "TAIL");
    expect(block.state).toBe("streaming");
    expect(fetches).toBe(0);
  });
});
