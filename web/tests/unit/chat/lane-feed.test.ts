import { describe, expect, test } from "vitest";
import { LaneFeed, mergeLaneEntries } from "#lib/screens/chat/laneFeed.svelte.js";
import { laneModel } from "#lib/screens/chat/lane.js";
import { sha256Hex } from "#lib/stores/blocks.js";
import {
  ChatStore,
  type ChatApi,
  type HistoryBlock,
  type LanePage,
} from "#lib/stores/chat.svelte.js";
import { buildRows } from "#lib/timeline/rows.js";
import type { HubEvent } from "#lib/sync/wire.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { OpJournal } from "#lib/ops/journal.js";

const CONV = "00000000-0000-4000-8000-0000000000aa";
const ROOT = "00000000-0000-4000-8000-000000000a01";

type Item = Omit<HistoryBlock, "kind">;

function seqOf(n: number): string {
  return String(n);
}

function item(id: string, n: number, over: Partial<Item> = {}): Item {
  return {
    id,
    block_kind: "tool_call",
    content: "",
    content_bytes: 0,
    content_truncated: false,
    created_seq: seqOf(n),
    created_at: "2026-11-01T09:00:00.000Z",
    rev: 1,
    state: "final",
    parent_block_id: ROOT,
    agent: "general-purpose",
    model: "claude-sonnet-5-5",
    tool_name: "Bash",
    summary: `step ${n}`,
    input_command_id: "00000000-0000-4000-8000-0000000000c1",
    ...over,
  };
}

const root = (): Item =>
  item(ROOT, 5, {
    parent_block_id: null,
    agent: "orchestrator",
    model: "claude-opus-5-5",
    tool_name: "Agent",
    summary: "Разобрать падение",
  });

/** Лента субагента на 250 блоков, которую хаб отдаёт страницами от нового к старому. */
function hubLane(total: number, size = 100): { api: NonNullable<ChatApi["lane"]>; calls: { before?: string }[] } {
  const all = Array.from({ length: total }, (_, index) => item(`00000000-0000-4000-8000-${String(index).padStart(12, "0")}`, 10 + index));
  const calls: { before?: string }[] = [];
  return {
    calls,
    api: async (_id, _block, query): Promise<LanePage> => {
      calls.push({ before: query?.before });
      const below = query?.before ? all.filter((it) => Number(it.created_seq) < Number(query.before)) : all;
      const page = below.slice(-size);
      const first = page[0];
      const more = below.length > page.length;
      return { root: root(), items: page, next_before: more && first ? first.created_seq : null };
    },
  };
}

function chatOf(lane?: ChatApi["lane"]): ChatStore {
  const api: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: async () => ({ items: [] }),
    lane,
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
  };
  return new ChatStore(CONV, {
    api,
    ops: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: { subscribe: () => ({ release() {}, status: "idle", cursor: "0" }) } as unknown as ConnectionManager,
    blockIO: {
      fetchContent: async () => ({ content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 }),
      sha256Hex,
    },
    clock: { now: () => 0 },
    probe: {},
  });
}

function started(id: string, n: number, parent: string): HubEvent {
  return {
    v: 1,
    id: "00000000-0000-4000-8000-0000000000ee",
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: `conversation:${CONV}`,
    scope_seq: String(n),
    scope_seq_from: String(n),
    committed_at: "2026-11-01T09:10:00.000Z",
    committed_at_from: "2026-11-01T09:10:00.000Z",
    conversation_id: CONV,
    execution_id: null,
    kind: "block_started",
    payload: {
      block_id: id,
      rev: 0,
      kind: "tool_call",
      input_command_id: null,
      parent_block_id: parent,
      agent: "general-purpose",
      model: "claude-sonnet-5-5",
      tool_name: "Read",
      summary: "live step",
      approved_by: null,
    },
  } as unknown as HubEvent;
}

describe("lane feed", () => {
  test("lane_outside_the_loaded_window_gets_its_steps_from_the_hub", async () => {
    const hub = hubLane(30);
    const chat = chatOf(hub.api);
    const feed = new LaneFeed(chat, ROOT);
    expect(buildRows(feed.entries, { laneRoot: ROOT })).toHaveLength(0);
    expect(feed.root).toBeUndefined();
    await feed.start();
    expect(feed.load).toBe("ready");
    expect(feed.root?.id).toBe(ROOT);
    const steps = feed.entries.filter((entry) => entry.kind === "block" && entry.block.parentId === ROOT);
    expect(steps).toHaveLength(30);
    expect(buildRows(feed.entries, { laneRoot: ROOT }).length).toBeGreaterThan(0);
    expect(chat.timeline.entries).toHaveLength(0);
  });

  test("a_lane_of_250_blocks_is_read_in_bounded_pages_newest_first", async () => {
    const hub = hubLane(250);
    const chat = chatOf(hub.api);
    const feed = new LaneFeed(chat, ROOT);
    await feed.start();
    expect(feed.hasOlder).toBe(true);
    expect(feed.entries.filter((entry) => entry.key !== `block:${ROOT}`)).toHaveLength(100);
    await feed.loadOlder();
    await feed.loadOlder();
    expect(feed.hasOlder).toBe(false);
    expect(hub.calls.map((call) => call.before)).toEqual([undefined, "160", "60"]);
    const seqs = feed.entries.map((entry) => Number(entry.seq));
    expect(seqs).toEqual([...seqs].sort((a, b) => a - b));
    expect(feed.entries).toHaveLength(251);
    await feed.loadOlder();
    expect(hub.calls).toHaveLength(3);
  });

  test("live_blocks_of_a_running_lane_join_the_loaded_ones", async () => {
    const chat = chatOf(hubLane(3).api);
    const feed = new LaneFeed(chat, ROOT);
    await feed.start();
    chat.applier.applyEvents([{ event: started("live".padEnd(36, "0"), 999, ROOT), overlap: false }]);
    const keys = feed.entries.map((entry) => entry.key);
    expect(keys.at(-1)).toBe(`block:${"live".padEnd(36, "0")}`);
    const rows = buildRows(feed.entries, { laneRoot: ROOT });
    expect(rows.flatMap((row) => row.entries).some((entry) => entry.key.includes("live"))).toBe(true);
  });

  test("an_open_lane_is_live_while_the_conversation_feed_is_detached_from_the_tail", async () => {
    const chat = chatOf(hubLane(3).api);
    const feed = new LaneFeed(chat, ROOT);
    await feed.start();
    chat.timeline.tailDetached = true;
    const inLane = "live".padEnd(36, "0");
    const nested = "deep".padEnd(36, "0");
    const foreign = "other".padEnd(36, "0");
    chat.applier.applyEvents([
      { event: started(inLane, 900, ROOT), overlap: false },
      { event: started(nested, 901, inLane), overlap: false },
      { event: started(foreign, 902, "unrelated".padEnd(36, "0")), overlap: false },
    ]);
    expect(chat.blocks.has(inLane)).toBe(true);
    expect(chat.blocks.has(nested)).toBe(true);
    // Чужой блок живёт в модели, но в ленту не встаёт: оторванное окно его только считает.
    expect(chat.blocks.has(foreign)).toBe(true);
    expect(chat.timeline.entries.map((entry) => entry.key)).not.toContain(`block:${foreign}`);
    const keys = feed.entries.map((entry) => entry.key);
    expect(keys).toContain(`block:${inLane}`);
    expect(keys).toContain(`block:${nested}`);
    expect(keys).not.toContain(`block:${foreign}`);
    expect(chat.timeline.entries.map((entry) => entry.key)).not.toContain(`block:${inLane}`);
    expect(buildRows(feed.entries, { laneRoot: ROOT }).flatMap((row) => row.entries).some((entry) => entry.key.includes("live"))).toBe(true);
    const delta = {
      ...started(inLane, 904, ROOT),
      kind: "block_delta",
      payload: { block_id: inLane, base_rev: 0, rev: 1, append: "токен" },
    } as unknown as HubEvent;
    chat.applier.applyEvents([{ event: delta, overlap: false }]);
    expect(chat.blocks.get(inLane)?.rev).toBe(1);
    feed.stop();
    expect(chat.laneLive).toHaveLength(0);
    chat.applier.applyEvents([{ event: started("late".padEnd(36, "0"), 903, ROOT), overlap: false }]);
    expect(chat.laneLive).toHaveLength(0);
  });

  test("a_live_revision_newer_than_the_page_is_kept", async () => {
    const chat = chatOf(async () => ({ root: root(), items: [item("x1".padEnd(36, "0"), 11, { rev: 1, content: "old" })] }));
    chat.applier.applyEvents([{ event: started("x1".padEnd(36, "0"), 1, ROOT), overlap: false }]);
    const block = chat.blocks.get("x1".padEnd(36, "0"))!;
    block.rev = 4;
    block.text = "newer";
    await new LaneFeed(chat, ROOT).start();
    expect(block.text).toBe("newer");
  });

  test("a_failed_read_can_be_retried", async () => {
    let fail = true;
    const hub = hubLane(2);
    const chat = chatOf(async (...args) => {
      if (fail) throw new Error("offline");
      return hub.api(...args);
    });
    const feed = new LaneFeed(chat, ROOT);
    await feed.start();
    expect(feed.load).toBe("failed");
    fail = false;
    await feed.start();
    expect(feed.load).toBe("ready");
    expect(feed.root?.id).toBe(ROOT);
  });

  test("merge_keeps_one_entry_per_block_in_order", () => {
    const a = { kind: "block" as const, key: "block:a", seq: "2", block: {} as never };
    const b = { kind: "block" as const, key: "block:b", seq: "10", block: {} as never };
    const c = { kind: "block" as const, key: "block:c", seq: "3", block: {} as never };
    expect(mergeLaneEntries([a, b], [c, a]).map((entry) => entry.key)).toEqual(["block:a", "block:c", "block:b"]);
    const live = [a, b];
    expect(mergeLaneEntries(live, [])).toBe(live);
  });

  test("an_unknown_lane_root_is_loading_not_done", () => {
    expect(laneModel([], ROOT, { load: "loading" }).status).toBe("Загружаю…");
    expect(laneModel([], ROOT).status).toBe("Загружаю…");
    expect(laneModel([], ROOT, { load: "failed" }).status).toBe("Не удалось загрузить");
    expect(laneModel([], ROOT, { load: "loading" }).running).toBe(false);
  });
});
