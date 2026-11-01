import { describe, expect, it } from "vitest";
import type { DraftStore } from "#lib/drafts/store.js";
import type { OpJournal } from "#lib/ops/journal.js";
import { sha256Hex } from "#lib/stores/blocks.js";
import { ChatStore, type ChatApi } from "#lib/stores/chat.svelte.js";
import { freshInput } from "#lib/stores/inputs.js";
import { LOCAL_SEQ, TimelineModel, type TimelineEntry } from "#lib/stores/timeline.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import type { HubEvent } from "#lib/sync/wire.js";

const CONV = "00000000-0000-4000-8000-0000000000aa";
const INPUT = "00000000-0000-4000-8000-0000000000c1";

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

function chatOf(history?: ChatApi["history"]): ChatStore {
  const api: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: history ?? (async () => ({ items: [] })),
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
  };
  const chat = new ChatStore(CONV, {
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
  chat.activity = { state: "idle" };
  return chat;
}

function send(chat: ChatStore, key: string) {
  chat.admitLocal(freshInput(key, "привет", 1));
  return chat.inputs.get(key)!;
}

function queued(key: string, seq = "4"): { event: HubEvent; overlap: boolean } {
  return {
    event: ev("input_queued", { input_id: INPUT, input_seq: seq, idempotency_key: key, state: "queued", content: [{ type: "text", text: "привет" }] }, seq),
    overlap: false,
  };
}

function state(next: string, seq: string): { event: HubEvent; overlap: boolean } {
  return { event: ev("input_state_changed", { input_id: INPUT, state: next }, seq), overlap: false };
}

function keys(chat: ChatStore): string[] {
  return chat.timeline.entries.map((entry) => entry.key);
}

describe("optimistic send", () => {
  it("an_idle_session_puts_the_bubble_into_the_feed_at_once_and_leaves_the_strip_empty", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    expect(input.placement).toBe("feed");
    expect(input.optimistic).toBe(true);
    expect(input.state).toBe("saving");
    expect(keys(chat)).toEqual(["input:k1"]);
    expect(chat.strip()).toEqual([]);
  });

  it("a_running_turn_keeps_the_message_in_the_strip", () => {
    const chat = chatOf();
    chat.activity = { state: "working" };
    const input = send(chat, "k1");
    expect(input.placement).toBe("strip");
    expect(input.optimistic).toBe(false);
    expect(keys(chat)).toEqual([]);
    expect(chat.strip().map((item) => item.key)).toEqual(["k1"]);
  });

  it("an_unknown_activity_keeps_the_message_in_the_strip", () => {
    const chat = chatOf();
    chat.activity = null;
    expect(send(chat, "k1").placement).toBe("strip");
  });

  it.each([
    ["a_pause", (chat: ChatStore) => { chat.queue.pause_reasons = [{ reason: "limit_reached" }]; }],
    ["a_hold", (chat: ChatStore) => { chat.queue.hold_reasons = [{ reason: "weekly_reserve" }]; }],
    ["a_busy_slot", (chat: ChatStore) => { chat.queue.run_slot = { running: { input_id: "other" } }; }],
    ["a_queued_message", (chat: ChatStore) => { chat.queue.queued = 1; }],
  ])("%s_keeps_the_message_in_the_strip", (_name, arrange) => {
    const chat = chatOf();
    arrange(chat);
    expect(send(chat, "k1").placement).toBe("strip");
    expect(keys(chat)).toEqual([]);
  });

  it("a_second_message_waits_behind_the_first_one_still_on_its_way", () => {
    const chat = chatOf();
    send(chat, "k1");
    expect(send(chat, "k2").placement).toBe("strip");
    expect(chat.strip().map((item) => item.key)).toEqual(["k2"]);
  });

  it("a_session_in_the_middle_of_a_history_page_keeps_the_message_in_the_strip", () => {
    const chat = chatOf();
    chat.timeline.tailDetached = true;
    expect(send(chat, "k1").placement).toBe("strip");
    expect(chat.timeline.newerCount).toBe(0);
  });

  it("server_stages_reconcile_in_place_without_a_duplicate_or_a_strip_chip", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([queued("k1")]);
    expect(input.state).toBe("queued");
    expect(input.optimistic).toBe(true);
    expect(chat.strip()).toEqual([]);
    expect(keys(chat)).toEqual(["input:k1"]);
    chat.applier.applyEvents([state("dispatched", "5"), state("on_machine", "6")]);
    expect(input.state).toBe("on_machine");
    expect(chat.strip()).toEqual([]);
    expect(keys(chat)).toEqual(["input:k1"]);
    expect(chat.timeline.entries[0]!.seq).not.toBe("10");
    chat.applier.applyEvents([state("transmitting", "10")]);
    expect(input.feedSeq).toBe("10");
    expect(input.optimistic).toBe(false);
    expect(keys(chat)).toEqual(["input:k1"]);
    expect(chat.timeline.entries.map((entry) => entry.seq)).toEqual(["10"]);
    expect(chat.inputs.get("k1")).toBe(input);
  });

  it("a_pause_after_the_send_moves_the_bubble_to_the_strip_where_it_can_be_cancelled", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([queued("k1")]);
    expect(keys(chat)).toEqual(["input:k1"]);
    chat.applier.applyEvents([
      {
        event: ev("queue_state_changed", { pause_reasons: [{ reason: "machine_lost" }], hold_reasons: [], run_slot: "free" }, "5"),
        overlap: false,
      },
    ]);
    expect(input.optimistic).toBe(false);
    expect(chat.strip()).toEqual([input]);
    expect(keys(chat)).toEqual([]);
  });

  it("a_queue_resume_with_a_reason_left_moves_the_bubble_as_well", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([queued("k1")]);
    chat.applier.applyEvents([
      { event: ev("queue_resumed", { pause_reasons: [{ reason: "limit_reached" }] }, "5"), overlap: false },
    ]);
    expect(input.optimistic).toBe(false);
    expect(keys(chat)).toEqual([]);
  });

  it("an_own_slot_keeps_the_bubble_in_place", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([queued("k1")]);
    chat.applier.applyEvents([
      {
        event: ev("queue_state_changed", { pause_reasons: [], hold_reasons: [], run_slot: { dispatching: { input_id: INPUT, attempt: 1 } } }, "5"),
        overlap: false,
      },
    ]);
    expect(input.optimistic).toBe(true);
    expect(keys(chat)).toEqual(["input:k1"]);
  });

  it("a_snapshot_without_the_queued_input_removes_its_bubble", () => {
    const chat = chatOf();
    send(chat, "k1");
    chat.applier.applyEvents([queued("k1")]);
    chat.applier.applySnapshot({ queue: { queued: [] } });
    expect(chat.inputs.get("k1")).toBeUndefined();
    expect(keys(chat)).toEqual([]);
  });

  it("the_bubble_never_claims_a_stage_the_hub_has_not_reported", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    expect(input.state).toBe("saving");
    chat.applier.applyEvents([queued("k1")]);
    expect(input.state).toBe("queued");
    expect(input.marks.accepted).toBe(false);
  });

  it("a_pause_found_at_queueing_moves_the_bubble_back_to_the_strip", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([
      {
        event: ev("queue_state_changed", { pause_reasons: [{ reason: "limit_reached" }], hold_reasons: [], run_slot: "free" }, "3"),
        overlap: false,
      },
      queued("k1"),
    ]);
    expect(input.placement).toBe("strip");
    expect(input.optimistic).toBe(false);
    expect(keys(chat)).toEqual([]);
    expect(chat.strip().map((item) => item.key)).toEqual(["k1"]);
  });

  it("a_rejection_before_transmission_keeps_the_bubble_with_its_recovery", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([queued("k1"), state("rejected", "9")]);
    expect(input.state).toBe("rejected");
    expect(input.optimistic).toBe(false);
    expect(chat.timeline.entries.map((entry) => entry.seq)).toEqual(["9"]);
  });

  it("a_cancel_removes_the_bubble_and_shows_the_note_in_the_strip", () => {
    const chat = chatOf();
    const input = send(chat, "k1");
    chat.applier.applyEvents([queued("k1"), state("cancelled", "8")]);
    expect(input.optimistic).toBe(false);
    expect(keys(chat)).toEqual([]);
    expect(chat.strip().map((item) => item.key)).toEqual(["k1"]);
  });

  it("a_tail_reload_keeps_the_local_bubble_below_the_loaded_history", async () => {
    const chat = chatOf(async () => ({
      items: [
        {
          kind: "block",
          id: "b1",
          block_kind: "text",
          content: "ответ",
          content_truncated: false,
          content_bytes: 10,
          state: "final",
          rev: 1,
          created_seq: "2",
          created_at: "2026-10-28T12:00:00.000Z",
          parent_block_id: null,
          agent: "main",
          model: "m",
        },
      ],
    }) as never);
    send(chat, "k1");
    await chat.reloadTail();
    expect(keys(chat)).toEqual(["block:b1", "input:k1"]);
  });

  it("local_entries_do_not_become_paging_cursors", () => {
    const chat = chatOf();
    send(chat, "k1");
    expect(chat.timeline.newestSeq()).toBeNull();
    expect(chat.timeline.oldestSeq()).toBeNull();
  });

  it("discarding_a_local_message_clears_both_the_map_and_the_feed", () => {
    const chat = chatOf();
    send(chat, "k1");
    chat.discardLocal("k1");
    expect(chat.inputs.has("k1")).toBe(false);
    expect(keys(chat)).toEqual([]);
  });

  it("every_own_send_bumps_the_serial_the_feed_follows", () => {
    const chat = chatOf();
    const before = chat.ownSends;
    send(chat, "k1");
    expect(chat.ownSends).toBe(before + 1);
  });
});

describe("attachments of an own message", () => {
  const ATT = "00000000-0000-4000-8000-0000000000a1";

  it("a_ready_event_without_a_name_does_not_blank_the_file_the_owner_just_attached", () => {
    const chat = chatOf();
    const view = freshInput("k1", "с файлом", 1, [
      { attachment_id: ATT, name: "shot.png", mime: "image/png", size: 4096, sha256: "", image: true },
    ]);
    chat.admitLocal(view);
    chat.applier.applyEvents([
      {
        event: ev("attachment_state_changed", { input_id: INPUT, attachment_id: ATT, machine_state: "pending", machine_reason: null }, "2"),
        overlap: false,
      },
      {
        event: ev(
          "input_queued",
          { input_id: INPUT, input_seq: "4", idempotency_key: "k1", state: "queued", content: [{ type: "text", text: "с файлом" }, { type: "attachment", attachment_id: ATT }] },
          "4",
        ),
        overlap: false,
      },
    ]);
    expect(chat.inputs.get("k1")!.attachments).toEqual([
      { attachment_id: ATT, name: "shot.png", mime: "image/png", size: 4096, sha256: "", image: true },
    ]);
  });

  it("a_message_from_another_device_takes_the_file_description_from_its_content_block", () => {
    const chat = chatOf();
    chat.applier.applyEvents([
      {
        event: ev(
          "input_queued",
          {
            input_id: INPUT,
            input_seq: "4",
            idempotency_key: "k2",
            state: "queued",
            content: [{ type: "attachment", attachment_id: ATT, name: "clip.mp4", mime: "video/mp4", size: 9000, sha256: "ab", image: false }],
          },
          "4",
        ),
        overlap: false,
      },
    ]);
    expect(chat.inputs.get("k2")!.attachments[0]).toMatchObject({ name: "clip.mp4", mime: "video/mp4", size: 9000 });
  });
});

describe("timeline local entries", () => {
  const block = (seq: string): TimelineEntry =>
    ({ kind: "block", key: `block:${seq}`, seq, block: { createdSeq: seq } as never }) as TimelineEntry;
  const local = (key: string): TimelineEntry =>
    ({ kind: "input", key: `input:${key}`, seq: LOCAL_SEQ, input: freshInput(key, "x", 1) }) as TimelineEntry;

  it("older_and_newer_pages_never_slip_in_behind_a_local_entry", () => {
    const timeline = new TimelineModel();
    timeline.applyHistoryPage([block("5")], "tail");
    timeline.upsert(local("k1"));
    timeline.applyHistoryPage([block("6")], "newer");
    timeline.applyHistoryPage([block("1")], "older");
    expect(timeline.entries.map((entry) => entry.key)).toEqual(["block:1", "block:5", "block:6", "input:k1"]);
  });

  it("a_real_input_from_a_page_replaces_the_local_one_by_key", () => {
    const timeline = new TimelineModel();
    timeline.upsert(local("k1"));
    const real = { ...local("k1"), seq: "7" } as TimelineEntry;
    timeline.applyHistoryPage([block("6"), real], "tail");
    expect(timeline.entries.map((entry) => [entry.key, entry.seq])).toEqual([
      ["block:6", "6"],
      ["input:k1", "7"],
    ]);
  });

  it("a_block_arriving_before_the_server_seq_lands_above_the_local_bubble", () => {
    const timeline = new TimelineModel();
    timeline.upsert(local("k1"));
    timeline.upsert(block("3"));
    expect(timeline.entries.map((entry) => entry.key)).toEqual(["block:3", "input:k1"]);
  });
});
