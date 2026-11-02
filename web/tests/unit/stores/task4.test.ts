import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { flushSync } from "svelte";
import { SvelteMap } from "svelte/reactivity";
import { DraftStore } from "#lib/drafts/store.js";
import { OpJournal, type Op } from "#lib/ops/journal.js";
import { openDb } from "#lib/storage/idb.js";
import { createRuntime } from "#lib/app/runtime.js";
import {
  BlockModel,
  BlockSync,
  applyBlockEvent,
  sha256Hex,
  utf8Length,
  utf8Slice,
  type BlockIO,
  type BlockPage,
} from "#lib/stores/blocks.js";
import {
  ChatStore,
  type ChatApi,
  type HistoryBlock,
  type HistoryItem,
} from "#lib/stores/chat.svelte.js";
import {
  buildDelegation,
  laneBlocks,
  subagentVerb,
} from "#lib/stores/delegation.js";
import { fmt } from "#lib/format/ru.js";
import {
  freshInput,
  placeInput,
  type AttachmentRef,
  type InputUiState,
} from "#lib/stores/inputs.js";
import { limitView, windowText, type LimitWindow } from "#lib/stores/limits.js";
import { ListStore, type ListRow } from "#lib/stores/list.svelte.js";
import { MachinesStore } from "#lib/stores/machines.svelte.js";
import { TimelineModel, type TimelineEntry } from "#lib/stores/timeline.js";
import type { HubEvent } from "#lib/sync/wire.js";
import { ConnectionManager } from "#lib/sync/connection.js";
import {
  FakeClock,
  FakeHub,
  FakeHubApi,
  rememberDelegation,
  seedConversation,
  seedMachine,
} from "../../../testing/fake-hub";
import { TestLifecycle } from "../sync/helpers";
import { countEffectRuns } from "./effect-runs.svelte";

const CONV = "00000000-0000-4000-8000-0000000000aa";
const BLOCK = "00000000-0000-4000-8000-0000000000b1";

function ev(kind: string, payload: unknown, seq = "1"): HubEvent {
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

function started(
  rev = 0,
  id = BLOCK,
  kind = "text",
  extra: Record<string, unknown> = {},
): HubEvent {
  return ev("block_started", {
    block_id: id,
    rev,
    kind,
    input_command_id: null,
    parent_block_id: null,
    agent: "agent",
    model: "opus",
    tool_name: null,
    summary: null,
    approved_by: null,
    ...extra,
  });
}

function delta(
  base: number,
  rev: number,
  append: string,
  id = BLOCK,
  seq = String(rev),
): HubEvent {
  return ev("block_delta", { block_id: id, base_rev: base, rev, append }, seq);
}

function ioOf(
  pages: (blockId: string, offset: number) => BlockPage | Promise<BlockPage>,
): BlockIO {
  return {
    fetchContent: (blockId, offset) => Promise.resolve(pages(blockId, offset)),
    sha256Hex,
  };
}

async function drive(
  blocks: Map<string, BlockModel>,
  events: HubEvent[],
  io: BlockIO,
): Promise<BlockSync> {
  const sync = new BlockSync(blocks, io);
  for (const event of events) sync.handle(applyBlockEvent(blocks, event));
  await sync.settled;
  return sync;
}

function entry(seq: string, key = `block:${seq}`): TimelineEntry {
  const block = new BlockModel(key, "text");
  block.createdSeq = seq;
  block.state = "final";
  return { kind: "block", key, seq, block };
}

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("stores task 4", () => {
  it("delta_with_base_rev_equal_to_current_appends_and_advances_rev", async () => {
    const blocks = new Map<string, BlockModel>();
    await drive(
      blocks,
      [started(), delta(0, 1, "При"), delta(1, 2, "вет")],
      ioOf(() => {
        throw new Error("no_refetch");
      }),
    );
    const block = blocks.get(BLOCK)!;
    expect(block.text).toBe("Привет");
    expect(block.rev).toBe(2);
    expect(block.state).toBe("streaming");
  });

  it("duplicate_delta_does_not_double_text", async () => {
    const blocks = new Map<string, BlockModel>();
    const event = delta(0, 1, "аб");
    await drive(
      blocks,
      [started(), event, event, delta(0, 1, "аб")],
      ioOf(() => {
        throw new Error("no_refetch");
      }),
    );
    expect(blocks.get(BLOCK)!.text).toBe("аб");
  });

  it("rev_gap_marks_stale_and_refetches_content_then_continues", async () => {
    const blocks = new Map<string, BlockModel>();
    let release: ((page: BlockPage) => void) | undefined;
    let opened: (() => void) | undefined;
    const ready = new Promise<void>((resolve) => {
      opened = resolve;
    });
    const sync = new BlockSync(blocks, {
      sha256Hex,
      fetchContent: () =>
        new Promise((resolve) => {
          release = resolve;
          opened?.();
        }),
    });
    const apply = (event: HubEvent) =>
      sync.handle(applyBlockEvent(blocks, event));
    apply(started());
    apply(delta(0, 1, "A"));
    apply(delta(3, 4, "D", BLOCK, "4"));
    expect(blocks.get(BLOCK)!.state).toBe("stale");
    apply(delta(4, 5, "E", BLOCK, "5"));
    await ready;
    release?.({
      content: "ABC",
      offset: 0,
      rev: 3,
      state: "streaming",
      total_bytes: 3,
    });
    await sync.settled;
    expect(blocks.get(BLOCK)!.text).toBe("ABCDE");
    expect(blocks.get(BLOCK)!.rev).toBe(5);
    expect(blocks.get(BLOCK)!.state).toBe("streaming");
  });

  it("failed_refetch_does_not_stop_later_block_work_and_retries_with_backoff", async () => {
    const OTHER = "00000000-0000-4000-8000-0000000000b2";
    const blocks = new Map<string, BlockModel>();
    const fetched: string[] = [];
    let failFirst = true;
    const timers: { ms: number; fn: () => void }[] = [];
    const sync = new BlockSync(
      blocks,
      {
        sha256Hex,
        fetchContent: async (blockId) => {
          fetched.push(blockId);
          if (blockId === BLOCK && failFirst) {
            failFirst = false;
            throw new TypeError("Failed to fetch");
          }
          return { content: "ABC", offset: 0, rev: 3, state: "streaming", total_bytes: 3 };
        },
      },
      {
        schedule: (fn, ms) => {
          timers.push({ ms, fn });
          return () => undefined;
        },
      },
    );
    const apply = (event: HubEvent) => sync.handle(applyBlockEvent(blocks, event));
    apply(started());
    apply(delta(0, 1, "A"));
    apply(delta(3, 4, "D", BLOCK, "4"));
    await sync.settled.catch(() => undefined);
    expect(blocks.get(BLOCK)!.state).toBe("stale");

    apply(started(0, OTHER));
    apply(delta(0, 1, "A", OTHER, "5"));
    apply(delta(3, 4, "D", OTHER, "6"));
    await sync.settled;
    expect(fetched).toEqual([BLOCK, OTHER]);
    expect(blocks.get(OTHER)!.text).toBe("ABCD");

    expect(timers.map((timer) => timer.ms)).toEqual([1000]);
    timers[0]!.fn();
    await sync.settled;
    expect(fetched).toEqual([BLOCK, OTHER, BLOCK]);
    expect(blocks.get(BLOCK)!.text).toBe("ABCD");
    expect(blocks.get(BLOCK)!.state).toBe("streaming");
  });

  it("overlapping_merged_delta_marks_stale_and_refetches_instead_of_splitting", async () => {
    const blocks = new Map<string, BlockModel>();
    const chunks = Array.from({ length: 110 }, (_, index) =>
      String.fromCharCode(97 + (index % 26)),
    );
    const hub = chunks.join("");
    const events = [started()];
    for (let rev = 1; rev <= 105; rev++)
      events.push(delta(rev - 1, rev, chunks[rev - 1]!, BLOCK, String(rev)));
    let from = -1;
    const sync = new BlockSync(blocks, {
      sha256Hex,
      fetchContent: (_id, offset) => {
        from = offset;
        const tail = utf8Slice(hub, offset);
        return Promise.resolve({
          content: tail,
          offset,
          rev: 110,
          state: "streaming",
          total_bytes: hub.length,
        });
      },
    });
    for (const event of events) sync.handle(applyBlockEvent(blocks, event));
    await sync.settled;
    expect(blocks.get(BLOCK)!.rev).toBe(105);
    expect(blocks.get(BLOCK)!.bytes).toBe(105);
    sync.handle(
      applyBlockEvent(
        blocks,
        delta(102, 110, chunks.slice(102).join(""), BLOCK, "110"),
      ),
    );
    await sync.settled;
    expect(from).toBe(105);
    expect(blocks.get(BLOCK)!.text).toBe(hub);
    expect(blocks.get(BLOCK)!.rev).toBe(110);
  });

  it("late_delta_after_final_is_ignored", async () => {
    const blocks = new Map<string, BlockModel>();
    const checksum = await sha256Hex("готово");
    const sync = await drive(
      blocks,
      [
        started(),
        delta(0, 1, "черновик"),
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 2,
            text: "готово",
            checksum,
            text_omitted: false,
            summary: null,
          },
          "9",
        ),
      ],
      ioOf(() => {
        throw new Error("no_refetch");
      }),
    );
    const effects = applyBlockEvent(blocks, delta(2, 3, "лишнее", BLOCK, "10"));
    sync.handle(effects);
    await sync.settled;
    expect(blocks.get(BLOCK)!.text).toBe("готово");
    expect(blocks.get(BLOCK)!.state).toBe("final");
    expect(effects).toEqual([
      { type: "diag", kind: "delta_after_final", blockId: BLOCK },
    ]);
  });

  it("final_replaces_content_and_a_checksum_mismatch_refetches_and_records_a_diagnostic", async () => {
    const blocks = new Map<string, BlockModel>();
    const calls: number[] = [];
    const sync = await drive(
      blocks,
      [
        started(),
        delta(0, 1, "черновик"),
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 2,
            text: "не то",
            checksum: "00",
            text_omitted: false,
            summary: null,
          },
          "8",
        ),
      ],
      {
        sha256Hex,
        fetchContent: (_id, offset) => {
          calls.push(offset);
          return Promise.resolve({
            content: "верно",
            offset,
            rev: 2,
            state: "final",
            total_bytes: 5,
          });
        },
      },
    );
    expect(calls).toEqual([0]);
    expect(
      sync.diagnostics.some(
        (item) =>
          item.type === "diag" && item.kind === "block_checksum_mismatch",
      ),
    ).toBe(true);
    expect(blocks.get(BLOCK)!.text).toBe("верно");
  });

  it("final_with_text_omitted_verifies_the_accumulated_text_or_refetches_after_a_gap", async () => {
    const blocks = new Map<string, BlockModel>();
    const text = "накоплено";
    const checksum = await sha256Hex(text);
    await drive(
      blocks,
      [
        started(),
        delta(0, 1, text),
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 2,
            text: "",
            checksum,
            text_omitted: true,
            summary: null,
          },
          "4",
        ),
      ],
      ioOf(() => {
        throw new Error("no_refetch");
      }),
    );
    expect(blocks.get(BLOCK)!.state).toBe("final");
    expect(blocks.get(BLOCK)!.text).toBe(text);

    const gapped = new Map<string, BlockModel>();
    const calls: number[] = [];
    await drive(
      gapped,
      [
        started(),
        delta(0, 1, "A"),
        delta(4, 5, "E", BLOCK, "5"),
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 6,
            text: "",
            checksum,
            text_omitted: true,
            summary: null,
          },
          "6",
        ),
      ],
      {
        sha256Hex,
        fetchContent: (_id, offset) => {
          calls.push(offset);
          return Promise.resolve({
            content: "ABCDE",
            offset,
            rev: 6,
            state: "final",
            total_bytes: 5,
          });
        },
      },
    );
    expect(calls[0]).toBe(0);
    expect(gapped.get(BLOCK)!.text).toBe("ABCDE");
  });

  it("incomplete_keeps_partial_text_and_a_late_final_replaces_it", async () => {
    const blocks = new Map<string, BlockModel>();
    const checksum = await sha256Hex("целиком");
    await drive(
      blocks,
      [
        started(),
        delta(0, 1, "част"),
        ev(
          "block_incomplete",
          { block_id: BLOCK, rev: 2, reason: "прервано" },
          "3",
        ),
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 4,
            text: "целиком",
            checksum,
            text_omitted: false,
            summary: null,
          },
          "4",
        ),
      ],
      ioOf(() => {
        throw new Error("no_refetch");
      }),
    );
    expect(blocks.get(BLOCK)!.text).toBe("целиком");
    expect(blocks.get(BLOCK)!.state).toBe("final");
  });

  it("refetch_offset_is_the_utf8_length_of_the_applied_text", async () => {
    const text = "привет🙂中";
    const blocks = new Map<string, BlockModel>();
    let from = -1;
    await drive(blocks, [started(), delta(0, 1, text)], {
      sha256Hex,
      fetchContent: (_id, offset) => {
        from = offset;
        expect(() => utf8Slice(text, offset)).not.toThrow();
        expect(() => utf8Slice(text, 1)).toThrow("invalid_offset");
        const tail = utf8Slice(text + "ещё", offset);
        return Promise.resolve({
          content: tail,
          offset,
          rev: 2,
          state: "streaming",
          total_bytes: utf8Length(text + "ещё"),
        });
      },
    });
    const block = blocks.get(BLOCK)!;
    expect(block.bytes).toBe(utf8Length(text));
    const effects = applyBlockEvent(
      blocks,
      delta(0, 2, "перехлёст", BLOCK, "2"),
    );
    expect(effects).toContainEqual({
      type: "refetch",
      blockId: BLOCK,
      fromBytes: utf8Length(text),
    });
    const sync = new BlockSync(blocks, {
      sha256Hex,
      fetchContent: (_id, offset) => {
        from = offset;
        const tail = utf8Slice(text + "ещё", offset);
        return Promise.resolve({
          content: tail,
          offset,
          rev: 2,
          state: "streaming",
          total_bytes: utf8Length(text + "ещё"),
        });
      },
    });
    sync.handle(effects);
    await sync.settled;
    expect(from).toBe(utf8Length(text));
    expect(block.text).toBe(text + "ещё");
    expect(block.text.indexOf("приветпривет")).toBe(-1);
  });

  it("tool_call_partial_json_is_never_shown_as_an_object_until_final", async () => {
    const blocks = new Map<string, BlockModel>();
    const args = '{"path":"readme.md"}';
    const checksum = await sha256Hex(args);
    const sync = new BlockSync(
      blocks,
      ioOf(() => {
        throw new Error("no_refetch");
      }),
    );
    const apply = (event: HubEvent) =>
      sync.handle(applyBlockEvent(blocks, event));
    apply(
      started(0, BLOCK, "tool_call", { tool_name: "Read", summary: "Читает" }),
    );
    apply(delta(0, 1, '{"pa'));
    await sync.settled;
    expect(blocks.get(BLOCK)!.text).toBe("Читает");
    expect(() => JSON.parse(blocks.get(BLOCK)!.text)).toThrow();
    apply(
      ev(
        "block_final",
        {
          block_id: BLOCK,
          rev: 2,
          text: args,
          checksum,
          text_omitted: false,
          summary: "Читает",
        },
        "3",
      ),
    );
    await sync.settled;
    expect(JSON.parse(blocks.get(BLOCK)!.text)).toEqual({ path: "readme.md" });
  });

  it("snapshot_after_resync_replaces_streaming_text_without_duplication", () => {
    const chat = chatOf();
    chat.applier.applyEvents([
      { event: started(0, BLOCK, "text"), overlap: false },
      { event: delta(0, 1, "abc"), overlap: false },
    ]);
    const item: HistoryBlock = {
      kind: "block",
      id: BLOCK,
      block_kind: "text",
      content: "abcXYZ",
      content_bytes: 6,
      content_truncated: false,
      created_seq: "1",
      created_at: "2026-10-28T12:00:00.000Z",
      rev: 2,
      state: "streaming",
      agent: "agent",
      model: "opus",
    };
    chat.applier.applySnapshot({ history: { items: [item] } }, "2");
    expect(chat.blocks.get(BLOCK)!.text).toBe("abcXYZ");
    expect(
      chat.timeline.entries.filter((entry) => entry.key === `block:${BLOCK}`),
    ).toHaveLength(1);
  });

  it("random_event_interleavings_converge_to_reference_state", async () => {
    for (const seed of [1, 2, 3]) {
      const random = mulberry(seed);
      const blocks = new Map<string, BlockModel>();
      const canonical = new Map<
        string,
        { text: string; rev: number; checksum: string; finalized: boolean }
      >();
      const events: HubEvent[] = [];
      const alphabet = ["а", "б", "в", "🙂", "🚀", "中", "文", "x", "й", "\n"];

      for (let index = 0; index < 60; index++) {
        const id = `00000000-0000-4000-8000-${(index + 1).toString(16).padStart(12, "0")}`;
        const chunkCount = 4 + Math.floor(random() * 5);
        const chunks = Array.from(
          { length: chunkCount },
          () => alphabet[Math.floor(random() * alphabet.length)]!,
        );
        const text = chunks.join("");
        const checksum = await sha256Hex(text);
        let finalized = true;

        events.push(started(0, id, "text", {}));

        const middle: HubEvent[] = [];
        for (let rev = 1; rev <= chunks.length; rev++) {
          middle.push(
            delta(rev - 1, rev, chunks[rev - 1]!, id, `${index}:${rev}`),
          );
        }
        const last = middle[middle.length - 1]!;
        const kept = middle.slice(0, -1).filter(() => random() > 0.3);

        // Duplicates
        if (random() > 0.5 && kept.length > 1) {
          kept.push(kept[Math.floor(random() * kept.length)]!);
        }

        // Shuffle intermediate deltas
        for (let pass = kept.length - 1; pass > 0; pass--) {
          const swap = Math.floor(random() * (pass + 1));
          const current = kept[pass]!;
          kept[pass] = kept[swap]!;
          kept[swap] = current;
        }

        // Overlapping merged delta
        if (chunks.length > 3 && random() > 0.4) {
          const append = chunks.slice(1, 4).join("");
          kept.push(delta(1, 4, append, id, `${index}:merge`));
        }

        kept.push(last);
        events.push(...kept);

        // Terminal events
        const roll = random();
        if (roll < 0.2) {
          // Incomplete without late final
          events.push(
            ev(
              "block_incomplete",
              { block_id: id, rev: chunks.length, reason: "interrupted" },
              `${index}:term`,
            ),
          );
          finalized = false;
        } else if (roll < 0.4) {
          // Incomplete followed by late final
          events.push(
            ev(
              "block_incomplete",
              { block_id: id, rev: chunks.length, reason: "interrupted" },
              `${index}:term1`,
            ),
          );
          const textOmitted = random() > 0.5;
          events.push(
            ev(
              "block_final",
              {
                block_id: id,
                rev: chunks.length,
                checksum,
                text_omitted: textOmitted,
                text: textOmitted ? "" : text,
                summary: null,
              },
              `${index}:term2`,
            ),
          );
        } else {
          // Direct final (with text, text_omitted, or temporary checksum mismatch)
          const badChecksum = random() < 0.15;
          const textOmitted = random() > 0.5;
          events.push(
            ev(
              "block_final",
              {
                block_id: id,
                rev: chunks.length,
                checksum: badChecksum
                  ? "0000000000000000000000000000000000000000000000000000000000000000"
                  : checksum,
                text_omitted: textOmitted,
                text: textOmitted ? "" : text,
                summary: null,
              },
              `${index}:term`,
            ),
          );
        }

        canonical.set(id, { text, rev: chunks.length, checksum, finalized });
      }

      // Shuffle events across blocks to interleave streams
      for (let pass = events.length - 1; pass > 0; pass--) {
        const swap = Math.floor(random() * (pass + 1));
        const current = events[pass]!;
        events[pass] = events[swap]!;
        events[swap] = current;
      }
      events.sort((a, b) => {
        const aId = (a.payload as { block_id?: string }).block_id;
        const bId = (b.payload as { block_id?: string }).block_id;
        if (
          a.kind === "block_started" &&
          b.kind !== "block_started" &&
          aId === bId
        )
          return -1;
        if (
          b.kind === "block_started" &&
          a.kind !== "block_started" &&
          aId === bId
        )
          return 1;
        return 0;
      });

      const sync = new BlockSync(blocks, {
        sha256Hex,
        fetchContent: async (blockId, offset) => {
          const record = canonical.get(blockId)!;
          // Asynchronous resolution with jittered delay
          await new Promise((resolve) =>
            setTimeout(resolve, Math.floor(random() * 4)),
          );
          const content = utf8Slice(record.text, offset);
          return {
            content,
            offset,
            next_offset: null,
            rev: record.rev,
            checksum: record.checksum,
            state: record.finalized ? "final" : "streaming",
            total_bytes: utf8Length(record.text),
          };
        },
      });

      for (const event of events) sync.handle(applyBlockEvent(blocks, event));
      await sync.settled;

      for (const [id, record] of canonical) {
        const block = blocks.get(id)!;
        if (record.finalized) {
          expect(block.state).toBe("final");
          expect(block.text).toBe(record.text);
          expect(block.bytes).toBe(utf8Length(record.text));
        } else {
          expect(block.state).toBe("incomplete");
          expect(record.text.startsWith(block.text)).toBe(true);
        }
      }
    }
  });

  it("history_pages_prepend_without_duplicates_by_item_key", () => {
    const timeline = new TimelineModel();
    timeline.applyHistoryPage([entry("2"), entry("3")], "tail");
    timeline.applyHistoryPage([entry("1"), entry("2")], "older");
    expect(timeline.entries.map((item) => item.seq)).toEqual(["1", "2", "3"]);
  });

  it("input_queued_replaces_the_saving_item_by_idempotency_key_without_a_duplicate", () => {
    const key = "idem-1";
    const settled: string[] = [];
    const ops = {
      ops: new Map([
        [
          key,
          {
            key,
            kind: "input",
            conversationId: CONV,
            state: "saving",
            body: { content: [{ type: "text", text: "черновик" }] },
            createdAt: 1,
            attempts: 0,
          },
        ],
      ]),
      settle(id: string) {
        this.ops.delete(id);
        settled.push(id);
      },
      onOrphanRefused: () => () => undefined,
    };
    const chat = chatOf(ops as unknown as OpJournal);
    chat.open();
    expect(chat.strip().map((item) => item.key)).toEqual([key]);
    chat.applier.applyEvents([
      {
        event: ev(
          "input_queued",
          {
            input_id: BLOCK,
            input_seq: "4",
            idempotency_key: key,
            state: "queued",
            content: [{ type: "text", text: "черновик" }],
          },
          "4",
        ),
        overlap: false,
      },
    ]);
    expect([...chat.inputs.keys()]).toEqual([key]);
    expect(chat.inputs.get(key)!.state).toBe("queued");
    expect(chat.inputs.get(key)!.localOnly).toBe(false);
    expect(settled).toEqual([key]);
  });

  it("input_state_mapping_places_items_in_strip_or_feed", () => {
    const table: [InputUiState, "strip" | "feed" | "none"][] = [
      ["saving", "strip"],
      ["queued", "strip"],
      ["dispatched", "strip"],
      ["on_machine", "strip"],
      ["transmitting", "feed"],
      ["accepted", "feed"],
      ["acceptance_unconfirmed", "feed"],
      ["rejected", "feed"],
      ["cancelled", "none"],
    ];
    for (const [state, placement] of table)
      expect(placeInput(state)).toBe(placement);
  });

  it("input_queued_during_a_turn_enters_the_feed_after_that_turn_by_feed_seq", () => {
    const chat = chatOf();
    const key = "idem-turn";
    chat.applier.applyEvents([
      { event: started(0, BLOCK, "text"), overlap: false },
      { event: delta(0, 1, "ответ"), overlap: false },
      {
        event: ev(
          "input_queued",
          {
            input_id: "00000000-0000-4000-8000-0000000000c1",
            input_seq: "6",
            idempotency_key: key,
            state: "queued",
            content: [{ type: "text", text: "потом" }],
          },
          "6",
        ),
        overlap: false,
      },
    ]);
    expect(chat.timeline.entries.map((item) => item.kind)).toEqual(["block"]);
    expect(chat.strip().some((item) => item.key === key)).toBe(true);
    chat.applier.applyEvents([
      {
        event: ev(
          "input_state_changed",
          {
            input_id: "00000000-0000-4000-8000-0000000000c1",
            state: "transmitting",
          },
          "10",
        ),
        overlap: false,
      },
    ]);
    expect(chat.inputs.get(key)!.feedSeq).toBe("10");
    expect(chat.timeline.entries.map((item) => item.seq)).toEqual(["1", "10"]);
  });

  it("snapshot_queue_restores_queued_inputs_and_their_count_after_a_reload", () => {
    const chat = chatOf();
    const first = queuedView("idem-q1", "00000000-0000-4000-8000-0000000000c1");
    const second = queuedView("idem-q2", "00000000-0000-4000-8000-0000000000c2");
    chat.applier.applySnapshot(
      {
        queue: {
          queued: [first, second],
          pause_reasons: [],
          hold_reasons: [],
          run_slot: "free",
        },
      },
      "9",
    );
    expect(chat.queue.queued).toBe(2);
    expect(chat.strip().map((item) => [item.id, item.state, item.text])).toEqual(
      [
        [first.id, "queued", "idem-q1"],
        [second.id, "queued", "idem-q2"],
      ],
    );
    chat.applier.applyEvents([
      {
        event: ev(
          "input_state_changed",
          { input_id: first.id, state: "transmitting" },
          "10",
        ),
        overlap: false,
      },
    ]);
    expect(chat.timeline.entries.map((item) => item.key)).toEqual([
      "input:idem-q1",
    ]);
    expect(chat.strip().map((item) => item.key)).toEqual(["idem-q2"]);
  });

  it("snapshot_queue_settles_a_local_entry_the_post_already_committed", () => {
    const key = "idem-commit";
    const settled: string[] = [];
    const ops = {
      ops: new Map(),
      settle: (id: string) => settled.push(id),
    };
    const chat = chatOf(ops as unknown as OpJournal);
    chat.inputs.set(key, freshInput(key, "ещё сохраняется", 1));
    const view = queuedView(key, "00000000-0000-4000-8000-0000000000c3");
    chat.applier.applySnapshot({ queue: { queued: [view] } }, "9");
    expect(chat.inputs.get(key)?.id).toBe(view.id);
    expect(chat.inputs.get(key)?.state).toBe("queued");
    expect(settled).toEqual([key]);
  });

  it("snapshot_queue_drops_strip_rows_the_hub_no_longer_holds", () => {
    const chat = chatOf();
    const kept = queuedView("idem-kept", "00000000-0000-4000-8000-0000000000c4");
    const gone = queuedView("idem-gone", "00000000-0000-4000-8000-0000000000c5");
    chat.applier.applySnapshot({ queue: { queued: [kept, gone] } }, "9");
    chat.inputs.set("idem-local", freshInput("idem-local", "ещё не на хабе", 1));

    chat.applier.applySnapshot({ queue: { queued: [kept] } }, "10");

    expect(chat.queue.queued).toBe(1);
    expect(chat.strip().map((item) => item.key).sort()).toEqual([
      "idem-kept",
      "idem-local",
    ]);
  });

  it("strip_reads_without_writing_so_a_derived_can_call_it", () => {
    const key = "idem-orphan";
    const ops = {
      ops: new Map([
        [
          key,
          {
            key,
            kind: "input",
            conversationId: CONV,
            state: "unknown",
            body: { content: [{ type: "text", text: "из журнала" }] },
            createdAt: 1,
            attempts: 0,
          },
        ],
      ]),
      settle() {},
      onOrphanRefused: () => () => undefined,
    };
    const chat = chatOf(ops as unknown as OpJournal);
    expect(chat.strip()).toEqual([]);
    expect(chat.inputs.size).toBe(0);
    chat.open();
    expect(chat.strip().map((item) => item.text)).toEqual(["из журнала"]);
  });

  it("recovered_entry_keeps_its_text_and_leaves_when_the_hub_refuses_it", () => {
    const key = "idem-recovered";
    let refuse: (op: Op) => void = () => undefined;
    const op = {
      key,
      kind: "input",
      conversationId: CONV,
      state: "unknown",
      body: {
        content: [{ type: "text", text: "после перезагрузки" }],
        attachment_ids: ["att-1"],
      },
      createdAt: 1,
      attempts: 1,
    } as Op;
    const ops = {
      ops: new Map([[key, op]]),
      settle() {},
      onOrphanRefused(fn: (item: Op) => void) {
        refuse = fn;
        return () => undefined;
      },
    };
    const chat = chatOf(ops as unknown as OpJournal);
    chat.open();
    const [entry] = chat.strip();
    expect(entry?.text).toBe("после перезагрузки");
    expect(entry?.attachments.map((item) => item.attachment_id)).toEqual([
      "att-1",
    ]);

    ops.ops.delete(key);
    refuse(op);
    expect(chat.strip()).toEqual([]);
  });

  it("cancelled_input_leaves_the_feed_and_notes_in_the_strip_for_four_seconds", () => {
    const clock = new FakeClock(1);
    clock.advance(1_000);
    const chat = chatOf(undefined, clock);
    const key = "idem-cancel";
    const inputId = "00000000-0000-4000-8000-0000000000c2";
    chat.applier.applyEvents([
      {
        event: ev(
          "input_queued",
          {
            input_id: inputId,
            input_seq: "1",
            idempotency_key: key,
            state: "queued",
            content: [{ type: "text", text: "отмена" }],
          },
          "1",
        ),
        overlap: false,
      },
      {
        event: ev(
          "input_state_changed",
          { input_id: inputId, state: "accepted" },
          "2",
        ),
        overlap: false,
      },
      {
        event: ev(
          "input_state_changed",
          { input_id: inputId, state: "cancelled" },
          "3",
        ),
        overlap: false,
      },
    ]);
    expect(chat.timeline.entries.some((item) => item.kind === "input")).toBe(
      false,
    );
    expect(chat.strip().some((item) => item.key === key)).toBe(true);
    clock.advance(3999);
    expect(chat.strip().some((item) => item.key === key)).toBe(true);
    clock.advance(1);
    expect(chat.inputs.has(key)).toBe(false);
  });

  it("cancel_note_does_not_stick_after_leaving_the_chat", () => {
    const clock = new FakeClock(1);
    clock.advance(1_000);
    const chat = chatOf(undefined, clock);
    const queued = (key: string, id: string, seq: string) => ({
      event: ev(
        "input_queued",
        {
          input_id: id,
          input_seq: seq,
          idempotency_key: key,
          state: "queued",
          content: [{ type: "text", text: key }],
        },
        seq,
      ),
      overlap: false,
    });
    const cancelled = (id: string, seq: string) => ({
      event: ev("input_state_changed", { input_id: id, state: "cancelled" }, seq),
      overlap: false,
    });
    const first = "00000000-0000-4000-8000-0000000000d1";
    const second = "00000000-0000-4000-8000-0000000000d2";
    chat.applier.applyEvents([
      queued("early", first, "1"),
      queued("late", second, "2"),
      cancelled(first, "3"),
    ]);
    clock.advance(3000);
    chat.applier.applyEvents([cancelled(second, "4")]);
    chat.close();

    clock.advance(2000);
    chat.open();

    expect(chat.strip().map((item) => item.key)).toEqual(["late"]);
    clock.advance(2000);
    expect(chat.strip()).toEqual([]);
  });

  it("reopening_a_warm_chat_from_an_effect_does_not_tie_it_to_the_store", () => {
    const journal = new SvelteMap<string, Op>();
    const chat = chatOf({
      ops: journal,
      settle() {},
      onOrphanRefused: () => () => undefined,
    } as unknown as OpJournal);
    const effect = countEffectRuns(() => chat.open());
    flushSync();
    expect(effect.runs()).toBe(1);

    journal.set("k", {
      key: "k",
      kind: "input",
      conversationId: CONV,
      body: {},
      state: "saving",
      attempts: 0,
      createdAt: 1,
    });
    chat.inputs.set("k", freshInput("k", "текст", 1));
    flushSync();

    expect(effect.runs()).toBe(1);
    effect.stop();
  });

  it("queue_state_changed_updates_pauses_holds_and_run_slot", () => {
    const chat = chatOf();
    const hold = {
      reason: "quota_exhausted",
      provider: "anthropic",
      wake_at: "2026-10-30T06:00:00.000Z",
      window: "five_hour",
    };
    chat.applier.applyEvents([
      {
        event: ev(
          "queue_state_changed",
          {
            pause_reasons: [{ reason: "user_stop" }],
            hold_reasons: [hold],
            run_slot: { running: { input_id: BLOCK } },
          },
          "4",
        ),
        overlap: false,
      },
    ]);
    expect(chat.queue.pause_reasons).toEqual([{ reason: "user_stop" }]);
    expect(chat.queue.hold_reasons).toEqual([hold]);
    expect(chat.queue.run_slot).toEqual({ running: { input_id: BLOCK } });
    chat.applier.applyEvents([
      {
        event: ev(
          "queue_paused",
          { pause_reasons: [{ reason: "fatal_error" }] },
          "5",
        ),
        overlap: false,
      },
    ]);
    expect(chat.queuePausedCount).toBe(1);
    expect(chat.queue.pause_reasons).toEqual([{ reason: "user_stop" }]);
  });

  it("derived_changed_updates_list_and_chat_activity", () => {
    const list = listOf();
    const row = sampleRow("c1");
    list.applier.applySnapshot(
      {
        counts: emptyCounts(),
        groups: [],
        items: [row],
        limits: [],
        truncated: false,
      },
      "1",
    );
    const chat = chatOf();
    const activity = { state: "working", since: "2026-10-28T12:00:00.000Z" };
    const event = ev(
      "derived_changed",
      {
        conversation_id: "c1",
        activity,
        unread_count: 2,
        queue: { queued: 1, pause_reasons: [], hold_reasons: [] },
        last_activity_at: "2026-10-28T12:01:00.000Z",
        settings: { model: "opus", effort: "high", permission_mode: "default" },
        crunch: { enabled: true },
      },
      "2",
    );
    list.applier.applyEvents([{ event, overlap: false }]);
    chat.detail = { id: CONV, title: "Сессия" };
    chat.applier.applyEvents([
      {
        event: {
          ...event,
          payload: {
            ...(event as { payload: object }).payload,
            conversation_id: CONV,
          },
        } as HubEvent,
        overlap: false,
      },
    ]);
    expect(list.items.get("c1")!.activity).toEqual(activity);
    expect(list.items.get("c1")!.title).toBe("Сессия");
    expect(chat.activity).toEqual(activity);
  });

  it("limits_changed_updates_only_that_provider", () => {
    const list = listOf();
    list.limits = [
      {
        provider: "anthropic",
        five_hour: {
          used_permille: 10,
          resets_at: null,
          observed_at: "2026-10-28T12:00:00.000Z",
        },
        weekly: null,
        weekly_reserve_active: false,
      },
      {
        provider: "openai",
        five_hour: {
          used_permille: 20,
          resets_at: null,
          observed_at: "2026-10-28T12:00:00.000Z",
        },
        weekly: null,
        weekly_reserve_active: false,
      },
    ];
    list.applier.applyEvents([
      {
        event: ev(
          "limits_changed",
          {
            provider: "anthropic",
            window: "five_hour",
            used_permille: 340,
            resets_at: null,
            observed_at: "2026-10-28T12:05:00.000Z",
            weekly_reserve_active: false,
            weekly_reserve_resets_at: null,
          },
          "3",
        ),
        overlap: false,
      },
    ]);
    expect(
      list.limits.find((item) => item.provider === "anthropic")!.five_hour!
        .used_permille,
    ).toBe(340);
    expect(
      list.limits.find((item) => item.provider === "openai")!.five_hour!
        .used_permille,
    ).toBe(20);
  });

  it("limits_changed_monthly_updates_the_month_window", () => {
    const list = listOf();
    const observed = "2026-10-28T12:00:00.000Z";
    list.limits = [
      {
        provider: "opencode",
        five_hour: { used_permille: 10, resets_at: null, observed_at: observed },
        weekly: { used_permille: 20, resets_at: null, observed_at: observed },
        weekly_reserve_active: false,
      },
    ];
    list.applier.applyEvents([
      {
        event: ev(
          "limits_changed",
          {
            provider: "opencode",
            window: "monthly",
            used_permille: 93,
            resets_at: null,
            observed_at: "2026-10-28T12:05:00.000Z",
            weekly_reserve_active: false,
            weekly_reserve_resets_at: null,
          },
          "4",
        ),
        overlap: false,
      },
    ]);
    const provider = list.limits.find((item) => item.provider === "opencode")!;
    expect(provider.monthly!.used_permille).toBe(93);
    expect(provider.weekly!.used_permille).toBe(20);
  });

  it("limits_changed_without_a_value_marks_the_five_hour_window_absent_and_a_value_brings_it_back", () => {
    const list = listOf();
    const week = {
      used_permille: 540,
      resets_at: null,
      observed_at: "2026-10-28T12:00:00.000Z",
    };
    list.limits = [
      {
        provider: "claude",
        five_hour: { ...week, used_permille: 440 },
        weekly: week,
        weekly_reserve_active: false,
      },
    ];
    const change = (used: number | null, seq: string) => ({
      event: ev(
        "limits_changed",
        {
          provider: "claude",
          window: "five_hour",
          used_permille: used,
          resets_at: null,
          observed_at: "2026-10-28T12:05:00.000Z",
          weekly_reserve_active: false,
          weekly_reserve_resets_at: null,
        },
        seq,
      ),
      overlap: false,
    });
    list.applier.applyEvents([change(null, "4")]);
    const gone = list.limits.find((item) => item.provider === "claude")!;
    expect(gone.five_hour).toBeNull();
    expect(gone.five_hour_absent).toBe(true);
    expect(gone.weekly!.used_permille).toBe(540);
    list.applier.applyEvents([change(100, "5")]);
    const back = list.limits.find((item) => item.provider === "claude")!;
    expect(back.five_hour!.used_permille).toBe(100);
    expect(back.five_hour_absent).toBe(false);
  });

  it("conversation_updated_moves_items_between_filters", async () => {
    const list = listOf();
    list.applier.applySnapshot(
      {
        counts: emptyCounts(),
        groups: [],
        items: [sampleRow("c1")],
        limits: [],
        truncated: false,
      },
      "1",
    );
    expect((await list.search("", "all")).map((item) => item.id)).toEqual([
      "c1",
    ]);
    expect(await list.search("", "archived")).toEqual([]);
    list.applier.applyEvents([
      {
        event: ev(
          "conversation_updated",
          {
            ...sampleRow("c1"),
            archived: true,
            counts: { ...emptyCounts(), all: 0, archived: 1 },
          },
          "2",
        ),
        overlap: false,
      },
    ]);
    expect(await list.search("", "all")).toEqual([]);
    expect((await list.search("", "archived")).map((item) => item.id)).toEqual([
      "c1",
    ]);
  });

  it("unread_marker_is_posted_monotonic_and_throttled", () => {
    const reads: string[] = [];
    const clock = new FakeClock(1);
    const chat = chatOf(undefined, clock, async (_id, seq) => {
      reads.push(seq);
    });
    chat.markRead("5");
    chat.markRead("6");
    chat.markRead("4");
    expect(reads).toEqual(["5"]);
    clock.advance(2000);
    chat.markRead("6");
    chat.markRead("6");
    expect(reads).toEqual(["5", "6"]);
  });

  it("reserved_request_events_are_stored_and_never_break_reducers", () => {
    const chat = chatOf();
    chat.applier.applyEvents([
      {
        event: ev("request_opened", { request_id: "r1" }, "1"),
        overlap: false,
      },
      {
        event: ev("request_decided", { request_id: "r1" }, "2"),
        overlap: false,
      },
      {
        event: ev("request_closed", { request_id: "r1", reason: "applied" }, "3"),
        overlap: false,
      },
    ]);
    expect(chat.requests.items.size).toBe(1);
    expect(chat.requests.items.get("r1")?.state).toBe("applied");
    expect(chat.unknownKinds).toBe(0);
  });

  it("unknown_event_kinds_are_counted_and_ignored", () => {
    const chat = chatOf();
    expect(() =>
      chat.applier.applyEvents([
        {
          event: ev("not_a_client_kind", { secret: "нет" }, "1"),
          overlap: false,
        },
      ]),
    ).not.toThrow();
    expect(chat.unknownKinds).toBe(1);
    expect(chat.blocks.size).toBe(0);
  });

  it("delta_marks_and_diagnostics_stay_bounded_without_a_mounted_timeline", () => {
    const chat = chatOf();
    chat.applier.applyEvents([{ event: started(), overlap: false }]);
    const batch = [];
    for (let rev = 1; rev <= 3000; rev++)
      batch.push({ event: delta(rev - 1, rev, "x", BLOCK, String(rev)), overlap: false });
    for (let n = 0; n < 3000; n++)
      batch.push({
        event: ev("adapter_diagnostic", { code: "noise" }, String(4000 + n)),
        overlap: false,
      });
    chat.applier.applyEvents(batch);
    expect(chat.blocks.get(BLOCK)!.text).toHaveLength(3000);
    expect(chat.deltaMarks.length).toBeLessThanOrEqual(512);
    expect(chat.diagnostics.length).toBeLessThanOrEqual(200);
  });

  it("hidden_events_do_not_reach_the_reducers", () => {
    const chat = chatOf();
    chat.applier.applyEvents([
      { event: ev("hidden", {}, "1"), overlap: false },
    ]);
    expect(chat.hiddenCount).toBe(1);
    expect(chat.unknownKinds).toBe(0);
    expect(chat.blocks.size).toBe(0);
    expect(chat.requests.items.size).toBe(0);
  });

  it("list_events_merge_attribute_and_volatile_fields_independently", () => {
    const list = listOf();
    list.applier.applySnapshot(
      {
        counts: emptyCounts(),
        groups: [],
        items: [sampleRow("c1")],
        limits: [],
        truncated: false,
      },
      "1",
    );
    list.applier.applyEvents([
      {
        event: ev(
          "derived_changed",
          {
            conversation_id: "c1",
            activity: { state: "working", since: "2026-10-28T12:00:00.000Z" },
            unread_count: 4,
            queue: { queued: 2, pause_reasons: [], hold_reasons: [] },
            last_activity_at: null,
            settings: {
              model: "sonnet",
              effort: "low",
              permission_mode: "plan",
            },
            crunch: { enabled: true },
          },
          "2",
        ),
        overlap: false,
      },
    ]);
    list.applier.applyEvents([
      {
        event: ev(
          "conversation_updated",
          { ...sampleRow("c1"), title: "Новое имя", counts: emptyCounts() },
          "3",
        ),
        overlap: false,
      },
    ]);
    const row = list.items.get("c1")!;
    expect(row.title).toBe("Новое имя");
    expect(row.activity.state).toBe("working");
    expect(row.model).toBe("sonnet");
    expect(row.unread_count).toBe(4);
  });

  it("counts_refresh_from_conversation_created_updated_and_group_changed", () => {
    const list = listOf();
    const created = {
      ...sampleRow("c2"),
      counts: {
        all: 2,
        pinned: 1,
        archived: 0,
        groups: [{ id: "g1", count: 2 }],
      },
    };
    list.applier.applyEvents([
      { event: ev("conversation_created", created, "1"), overlap: false },
    ]);
    expect(list.counts.all).toBe(2);
    list.applier.applyEvents([
      {
        event: ev(
          "conversation_updated",
          {
            ...sampleRow("c2"),
            counts: { all: 2, pinned: 0, archived: 1, groups: [] },
          },
          "2",
        ),
        overlap: false,
      },
    ]);
    expect(list.counts.archived).toBe(1);
    list.applier.applyEvents([
      {
        event: ev(
          "group_changed",
          {
            group: null,
            groups: [
              {
                id: "g1",
                name: "Работа",
                position: 0,
                created_at: "2026-10-28T12:00:00.000Z",
              },
            ],
            counts: {
              all: 3,
              pinned: 0,
              archived: 1,
              groups: [{ id: "g1", count: 3 }],
            },
          },
          "3",
        ),
        overlap: false,
      },
    ]);
    expect(list.counts.groups).toEqual([{ id: "g1", count: 3 }]);
    expect(list.groups[0]!.name).toBe("Работа");
  });

  it("timeline_pages_forward_with_after_from_the_seam", async () => {
    const queries: { after?: string }[] = [];
    const chat = chatOf(undefined, undefined, undefined, async (_id, query) => {
      queries.push(query ?? {});
      return { items: [] };
    });
    chat.timeline.applyHistoryPage([entry("40")], "tail");
    await chat.loadNewer();
    expect(queries[0]?.after).toBe("40");
  });

  it("timeline_trims_the_newest_end_beyond_3000_items_and_detaches_the_tail", () => {
    const timeline = new TimelineModel();
    timeline.applyHistoryPage(
      Array.from({ length: 3001 }, (_, index) => entry(String(index + 1))),
      "older",
    );
    expect(timeline.entries).toHaveLength(3000);
    expect(timeline.tailDetached).toBe(true);
    expect(timeline.entries.at(-1)!.seq).toBe("3000");
    timeline.upsert(entry("3002"));
    expect(timeline.newerCount).toBe(1);
    expect(timeline.entries).toHaveLength(3000);
  });

  it("reload_tail_buffers_events_and_replays_them_after_the_page_under_rev_rules", async () => {
    let release: ((page: { items: HistoryItem[] }) => void) | undefined;
    const chat = chatOf(
      undefined,
      undefined,
      undefined,
      () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    );
    chat.applier.applyEvents([
      { event: started(), overlap: false },
      { event: delta(0, 1, "Hello"), overlap: false },
    ]);
    const pending = chat.reloadTail();
    chat.applier.applyEvents([{ event: delta(1, 2, "!"), overlap: false }]);
    expect(chat.blocks.get(BLOCK)!.text).toBe("Hello");
    const item: HistoryBlock = {
      kind: "block",
      id: BLOCK,
      block_kind: "text",
      content: "Hello",
      content_bytes: 5,
      content_truncated: false,
      created_seq: "1",
      created_at: "2026-10-28T12:00:00.000Z",
      rev: 1,
      state: "streaming",
    };
    release?.({ items: [item] });
    await pending;
    expect(chat.blocks.get(BLOCK)!.text).toBe("Hello!");
  });

  it("delegation_tree_from_blocks_matches_the_endpoint_nodes", async () => {
    const api = new FakeHubApi(new FakeHub());
    const conversationId = seedConversation(api);
    const rootId = api.id();
    const childId = api.id();
    const startedAt = Date.parse("2026-10-28T12:00:00.000Z");
    const root = new BlockModel(rootId, "tool_call");
    root.toolName = "Agent";
    root.agent = "orchestrator";
    root.model = "opus";
    root.summary = "найти";
    root.startedAt = startedAt;
    root.state = "final";
    const child = new BlockModel(childId, "tool_call");
    child.parentId = rootId;
    child.toolName = "Read";
    child.agent = "researcher";
    child.model = "opus";
    child.summary = "файл";
    child.startedAt = startedAt;
    child.state = "final";
    // Хаб отдаёт только вызовы субагентов; имя и модель берутся у первого дочернего блока другого агента.
    const expected = [
      {
        block_id: rootId,
        parent_block_id: null,
        started_at: new Date(startedAt).toISOString(),
        finished_at: null,
        state: "done",
        tool_name: "Agent",
        task: "найти",
        subagent: { name: "researcher", model: "opus" },
      },
    ];
    rememberDelegation(api, conversationId, expected);
    expect(buildDelegation([root, child])).toEqual(expected);
    expect(await api.delegation(conversationId)).toEqual(
      buildDelegation([root, child]),
    );
    expect(laneBlocks([root, child], rootId).map((block) => block.id)).toEqual([
      rootId,
      childId,
    ]);
  });

  it("subagent_verb_maps_tool_names_to_russian_verbs", () => {
    expect(subagentVerb("Read")).toBe("Читает");
    expect(subagentVerb("Grep")).toBe("Ищет");
    expect(subagentVerb("Glob")).toBe("Ищет");
    expect(subagentVerb("Bash")).toBe("Выполняет");
    expect(subagentVerb("Edit")).toBe("Правит");
    expect(subagentVerb("Write")).toBe("Правит");
    expect(subagentVerb("Agent")).toBe("Делегирует");
    expect(subagentVerb("Other")).toBe("Работает");
    expect(subagentVerb(null)).toBe("Работает");
  });

  it("machines_store_loads_the_snapshot_and_flags_stale_metrics", async () => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const fresh = seedMachine(api);
    fresh.metrics = { observed_at: new Date().toISOString() };
    fresh.warnings = [];
    const stale = seedMachine(api, {
      warnings: ["metrics_stale"],
      metrics: { observed_at: "2020-01-01T00:00:00.000Z" },
    });
    const clock = new FakeClock(1);
    const wire = hub.wire.memory(clock);
    const connection = new ConnectionManager({
      transport: wire,
      lifecycle: new TestLifecycle(),
      now: clock.now,
      schedule: clock.schedule,
      rng: clock.rng,
      raf: clock.raf,
      cancelRaf: clock.cancelRaf,
      probeAuth: async () => "ok",
      announce: () => undefined,
    });
    const store = new MachinesStore({
      connection,
      clock,
      api: {
        snapshot: () => api.machinesSnapshot(),
        events: (after, limit) => api.events("machines", after, limit),
      },
    });
    store.open();
    await clock.advanceAsync(50);
    expect(store.items.has(fresh.id)).toBe(true);
    expect(store.metricsStale(stale.id)).toBe(true);
    expect(store.metricsStale(fresh.id)).toBe(false);
    expect(store.catalog().some((item) => item.id === "claude")).toBe(true);
  });

  it("machines_store_takes_the_home_directory_from_the_report_event", async () => {
    const api = new FakeHubApi(new FakeHub());
    const machine = seedMachine(api);
    const store = new MachinesStore({
      connection: {} as ConnectionManager,
      clock: new FakeClock(1),
      api: {
        snapshot: () => api.machinesSnapshot(),
        events: async () => ({ events: [], head: "0" }) as never,
      },
    });
    store.applier.applySnapshot({ items: [machine] }, "1");
    expect(store.items.get(machine.id)?.home_dir ?? null).toBeNull();
    store.applier.applyEvents([
      {
        event: {
          kind: "machine_report_updated",
          payload: {
            machine_id: machine.id,
            worker_version: "1",
            capabilities: ["pilot_v1"],
            limits_ok: true,
            home_dir: "/home/owner",
          },
        },
      },
    ] as never);
    expect(store.items.get(machine.id)?.home_dir).toBe("/home/owner");
  });

  it("limit_view_marks_low_remaining_reserve_and_stale_observations", () => {
    const now = Date.parse("2026-10-28T12:00:00.000Z");
    const cfg = {
      stale_after_secs: 60,
      warn_pcts: [80, 95],
      weekly_reserve_pct: 95,
    };
    const fresh = "2026-10-28T11:59:30.000Z";
    const warning = limitView(
      {
        provider: "anthropic",
        five_hour: {
          used_permille: 850,
          resets_at: "2026-10-30T09:00:00.000Z",
          observed_at: fresh,
        },
        weekly: { used_permille: 100, resets_at: null, observed_at: fresh },
        weekly_reserve_active: false,
      },
      now,
      cfg,
    );
    expect(warning.tone).toBe("warning");
    expect(warning.five.text).toBe("15%");
    expect(warning.week.text).toBe("90%");
    expect(warning.stale).toBe(false);
    const reserve = limitView(
      {
        provider: "anthropic",
        weekly_reserve_active: true,
        five_hour: { used_permille: 10, resets_at: null, observed_at: fresh },
        weekly: { used_permille: 960, resets_at: null, observed_at: fresh },
      },
      now,
      cfg,
    );
    expect(reserve.tone).toBe("reserve");
    expect(reserve.week.text).toBe("4%");
    const stale = limitView(
      {
        provider: "anthropic",
        weekly_reserve_active: false,
        five_hour: {
          used_permille: 10,
          resets_at: null,
          observed_at: "2026-10-28T10:00:00.000Z",
        },
      },
      now,
      cfg,
    );
    expect(stale.stale).toBe(true);
    expect(stale.tone).toBe("default");
  });

  it("formatters_follow_the_mockup_strings", () => {
    const now = new Date(2026, 9, 26, 15, 0, 0).getTime();
    expect(fmt.pct(340)).toBe("34%");
    expect(fmt.pctApprox(4)).toBe("≈ 0,4%");
    expect(fmt.dayLabel(now, now)).toBe("Сегодня, 26 октября");
    expect(fmt.resetAt(new Date(2026, 9, 30, 9, 0, 0).getTime(), now)).toBe(
      "Пт, 30 октября · 09:00",
    );
    expect(fmt.duration(4 * 60 * 1000)).toBe("4 мин");
    expect(fmt.duration(2 * 60 * 1000 + 18 * 1000)).toBe("2 мин 18 с");
    expect(fmt.durationCoarse(2 * 60 * 1000 + 18 * 1000)).toBe("2 мин");
    expect(fmt.durationCoarse(61 * 60 * 1000 + 5000)).toBe("1 ч 1 мин");
    expect(fmt.durationCoarse(41_000)).toBe("41 с");
    expect(fmt.duration(200)).toBe("0,2 с");
    expect(fmt.worked(65 * 60 * 1000, 259 * 60 * 1000)).toBe(
      "1 ч 5 мин из 4 ч 19 мин",
    );
    expect(fmt.worked(372_000, 372_000)).toBe("6 мин");
    expect(fmt.worked(41_000, 120_000)).toBe("41 с из 2 мин");
    expect(fmt.clock(new Date(2026, 9, 26, 22, 14, 0).getTime())).toBe("22:14");
    expect(fmt.tokens(18420)).toBe("18 420");
    expect(fmt.bytes(12 * 1024)).toBe("12 КиБ");
    const forms: [string, string, string] = [
      "сообщение",
      "сообщения",
      "сообщений",
    ];
    expect(fmt.plural(1, forms)).toBe("сообщение");
    expect(fmt.plural(2, forms)).toBe("сообщения");
    expect(fmt.plural(5, forms)).toBe("сообщений");
    expect(fmt.plural(11, forms)).toBe("сообщений");
    expect(fmt.plural(21, forms)).toBe("сообщение");
    expect(fmt.ago(now - 2000, now)).toBe("2 секунды назад");
  });

  it("runtime_recovers_saving_operations_on_start_and_resubscribes_scopes", async () => {
    const owner = "00000000-0000-4000-8000-0000000000d1";
    const conversation = "00000000-0000-4000-8000-0000000000d2";
    const db = await openDb(owner);
    const key = "recover-me";
    await db.put("ops", key, {
      key,
      kind: "input",
      conversationId: conversation,
      body: { text: "сохранено" },
      state: "saving",
      attempts: 0,
      createdAt: 1,
    } satisfies Op);
    const hub = new FakeHub();
    const clock = new FakeClock(2);
    const sent: string[] = [];
    const posts: string[] = [];
    const sockets: { closeFromHub(code: number): void }[] = [];
    class Socket {
      static OPEN = 1;
      static CONNECTING = 0;
      static CLOSING = 2;
      static CLOSED = 3;
      readyState = 0;
      onopen: (() => void) | null = null;
      onmessage: ((event: { data: string }) => void) | null = null;
      onclose: ((event: { code: number; reason: string }) => void) | null =
        null;
      onerror: (() => void) | null = null;
      session?: ReturnType<FakeHub["attach"]>;
      constructor(url: string, protocol?: string) {
        if (!url.includes("/api/v1/ws") || protocol !== "canterlot.v1")
          throw new Error("bad_socket");
        sockets.push(this);
        queueMicrotask(() => {
          this.session = hub.attach({
            clock,
            message: (op) => this.onmessage?.({ data: JSON.stringify(op) }),
            close: (code) => this.closeFromHub(code),
          });
          this.readyState = Socket.OPEN;
          this.onopen?.();
        });
      }
      send(data: string): void {
        sent.push(data);
        if (this.session) hub.receive(this.session, JSON.parse(data));
      }
      close(code = 1000): void {
        this.closeFromHub(code);
      }
      closeFromHub(code: number): void {
        this.readyState = Socket.CLOSED;
        if (this.session) hub.detach(this.session);
        this.session = undefined;
        this.onclose?.({ code, reason: "" });
      }
    }
    const fetchImpl: typeof fetch = async (input, init) => {
      const url = String(input);
      if (url.includes("/api/v1/me"))
        return json({ login: "owner", owner_id: owner });
      if (url.includes("/snapshot/conversations")) {
        return json({
          scope: "conversations",
          cursor: "0",
          data: {
            counts: emptyCounts(),
            groups: [],
            items: [],
            limits: [],
            truncated: false,
          },
        });
      }
      if (url.includes("/api/v1/events"))
        return json({ events: [], head: "0" });
      if (init?.method === "POST") {
        posts.push(url);
        return json({ id: "sent" });
      }
      return json({});
    };
    const runtime = await createRuntime({
      fetch: fetchImpl,
      WebSocket: Socket as unknown as typeof WebSocket,
      indexedDB,
      clock,
      schedule: clock.schedule,
      rng: clock.rng,
    });
    await vi.waitFor(() => {
      expect(posts.some((url) => url.includes(conversation))).toBe(true);
    });
    await vi.waitFor(() =>
      expect(runtime.journal.ops.get(key)?.state).toBe("committed"),
    );
    const before = sockets.length;
    hub.chaos.cut();
    await clock.advanceAsync(1000);
    expect(sockets.length).toBeGreaterThan(before);
    expect(
      sent.filter((frame) => frame.includes('"op":"subscribe"')).length,
    ).toBeGreaterThan(1);
  });

  it("chat_store_survives_reconnect_mid_stream_with_exact_text", async () => {
    const clock = new FakeClock(3);
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const id = seedConversation(api);
    const wire = hub.wire.memory(clock);
    const connection = new ConnectionManager({
      transport: wire,
      lifecycle: new TestLifecycle(),
      now: clock.now,
      schedule: clock.schedule,
      rng: clock.rng,
      raf: clock.raf,
      cancelRaf: clock.cancelRaf,
      probeAuth: async () => "ok",
      announce: () => undefined,
    });
    const chat = new ChatStore(id, {
      api: adapt(api),
      ops: {
        ops: new Map(),
        settle() {},
        onOrphanRefused: () => () => undefined,
      } as unknown as OpJournal,
      drafts: {} as DraftStore,
      connection,
      blockIO: {
        fetchContent: (blockId, offset) => api.block(id, blockId, offset),
        sha256Hex,
      },
      clock,
      probe: {},
    });
    chat.open();
    await clock.advanceAsync(40);
    const blockId = api.id();
    const scope = `conversation:${id}`;
    hub.append(scope, {
      kind: "block_started",
      conversation_id: id,
      payload: {
        block_id: blockId,
        rev: 0,
        kind: "text",
        input_command_id: null,
        parent_block_id: null,
        agent: "agent",
        model: "opus",
        tool_name: null,
        summary: null,
        approved_by: null,
      },
    });
    hub.append(scope, {
      kind: "block_delta",
      conversation_id: id,
      payload: { block_id: blockId, base_rev: 0, rev: 1, append: "При" },
    });
    hub.flush();
    await clock.advanceAsync(40);
    expect(chat.blocks.get(blockId)!.text).toBe("При");
    hub.chaos.cut();
    hub.append(scope, {
      kind: "block_delta",
      conversation_id: id,
      payload: { block_id: blockId, base_rev: 1, rev: 2, append: "вет, мир" },
    });
    api.blocks.set(blockId, {
      text: "Привет, мир",
      rev: 2,
      state: "streaming",
    });
    await clock.advanceAsync(1500);
    expect(chat.blocks.get(blockId)!.text).toBe("Привет, мир");
  });

  it("asynchronous_fetch_content_interleaved_with_final_does_not_corrupt_body", async () => {
    const blocks = new Map<string, BlockModel>();
    let resolveFetch: ((page: BlockPage) => void) | undefined;
    const sync = new BlockSync(blocks, {
      fetchContent: (_id, offset) => {
        if (offset === 5) {
          return new Promise((resolve) => {
            resolveFetch = resolve;
          });
        }
        return Promise.resolve({
          content: "0123456789",
          offset: 0,
          rev: 10,
          state: "final",
          total_bytes: 10,
        });
      },
      sha256Hex,
    });
    sync.handle(applyBlockEvent(blocks, started(0, BLOCK)));
    sync.handle(applyBlockEvent(blocks, delta(0, 5, "01234")));
    // Overlapping delta triggers refetch from offset 5
    sync.handle(applyBlockEvent(blocks, delta(3, 7, "3456")));

    // block_final arrives while fetch is in-flight
    const finalChecksum = await sha256Hex("0123456789");
    sync.handle(
      applyBlockEvent(
        blocks,
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 10,
            text: "0123456789",
            checksum: finalChecksum,
            text_omitted: false,
            summary: null,
          },
          "4",
        ),
      ),
    );
    expect(blocks.get(BLOCK)!.text).toBe("0123456789");
    expect(blocks.get(BLOCK)!.state).toBe("final");

    // In-flight fetch resolves with older chunk
    resolveFetch?.({
      content: "56789",
      offset: 5,
      rev: 10,
      state: "streaming",
      total_bytes: 10,
    });
    await sync.settled;

    expect(blocks.get(BLOCK)!.text).toBe("0123456789");
    expect(blocks.get(BLOCK)!.state).toBe("final");
    expect(
      sync.diagnostics.filter(
        (d) => (d as { kind?: string }).kind === "block_checksum_mismatch",
      ),
    ).toHaveLength(0);
  });

  it("block_over_256_kib_refetched_across_pages", async () => {
    const blocks = new Map<string, BlockModel>();
    const totalBytes = 300 * 1024;
    const fullText = "x".repeat(totalBytes);
    const pageSize = 256 * 1024;
    const sync = new BlockSync(blocks, {
      fetchContent: async (_id, offset) => {
        const slice = fullText.slice(offset, offset + pageSize);
        const next_offset =
          offset + slice.length < totalBytes ? offset + slice.length : null;
        return {
          content: slice,
          offset,
          next_offset,
          rev: 5,
          state: "streaming",
          total_bytes: totalBytes,
        };
      },
      sha256Hex,
    });
    sync.handle(applyBlockEvent(blocks, started(0, BLOCK)));
    // Gap triggers refetch from 0
    sync.handle(applyBlockEvent(blocks, delta(2, 5, "...", BLOCK, "2")));
    await sync.settled;

    const block = blocks.get(BLOCK)!;
    expect(block.bytes).toBe(totalBytes);
    expect(block.body).toBe(fullText);
  });

  it("late_final_with_text_omitted_after_incomplete_transitions_to_final", async () => {
    const blocks = new Map<string, BlockModel>();
    const text = "Содержимое до сбоя";
    const checksum = await sha256Hex(text);
    const sync = new BlockSync(blocks, {
      fetchContent: async () => ({
        content: text,
        offset: 0,
        rev: 2,
        state: "final",
        total_bytes: utf8Length(text),
      }),
      sha256Hex,
    });
    sync.handle(applyBlockEvent(blocks, started(0, BLOCK)));
    sync.handle(applyBlockEvent(blocks, delta(0, 2, text)));
    sync.handle(
      applyBlockEvent(
        blocks,
        ev(
          "block_incomplete",
          { block_id: BLOCK, rev: 2, reason: "прервано" },
          "3",
        ),
      ),
    );
    expect(blocks.get(BLOCK)!.state).toBe("incomplete");

    // Late final with text_omitted: true and matching checksum
    sync.handle(
      applyBlockEvent(
        blocks,
        ev(
          "block_final",
          {
            block_id: BLOCK,
            rev: 2,
            checksum,
            text_omitted: true,
            summary: null,
          },
          "4",
        ),
      ),
    );
    await sync.settled;

    expect(blocks.get(BLOCK)!.state).toBe("final");
    expect(blocks.get(BLOCK)!.text).toBe(text);
  });

  it("forward_paging_beyond_3000_items_trims_oldest_end", () => {
    const timeline = new TimelineModel();
    const initial: TimelineEntry[] = Array.from({ length: 3000 }, (_, i) =>
      entry(String(i + 1)),
    );
    timeline.applyHistoryPage(initial, "tail");
    expect(timeline.entries).toHaveLength(3000);

    // User pages forward with 'newer'
    const newerItems: TimelineEntry[] = Array.from({ length: 50 }, (_, i) =>
      entry(String(3001 + i)),
    );
    timeline.applyHistoryPage(newerItems, "newer");

    expect(timeline.entries).toHaveLength(3000);
    expect(timeline.newestSeq()).toBe("3050");
    expect(timeline.oldestSeq()).toBe("51");
    expect(timeline.hasOlder).toBe(true);
  });

  it("snapshot_floor_with_sequences_9_10_11_uses_numeric_order", () => {
    const chat = chatOf();
    // Older entry '7' is final (terminal).
    // Timeline entry '9' was streaming (non-terminal) before snapshot arrived.
    const b7 = entry("7");
    const b9entry = new BlockModel("b9", "text");
    b9entry.createdSeq = "9";
    b9entry.state = "streaming";
    chat.timeline.applyHistoryPage(
      [b7, { kind: "block", key: "block:b9", seq: "9", block: b9entry }],
      "tail",
    );

    // Snapshot items: '9' (now updated to streaming text), '10' (final), '11' (final)
    const b9: HistoryBlock = {
      kind: "block",
      id: "b9",
      block_kind: "text",
      content: "девять",
      content_bytes: 12,
      content_truncated: false,
      created_seq: "9",
      created_at: "2026-10-28T12:00:00.000Z",
      rev: 1,
      state: "streaming",
    };
    const b10: HistoryBlock = {
      kind: "block",
      id: "b10",
      block_kind: "text",
      content: "десять",
      content_bytes: 12,
      content_truncated: false,
      created_seq: "10",
      created_at: "2026-10-28T12:00:01.000Z",
      rev: 1,
      state: "final",
    };
    const b11: HistoryBlock = {
      kind: "block",
      id: "b11",
      block_kind: "text",
      content: "одиннадцать",
      content_bytes: 20,
      content_truncated: false,
      created_seq: "11",
      created_at: "2026-10-28T12:00:02.000Z",
      rev: 1,
      state: "final",
    };

    chat.applier.applySnapshot({ history: { items: [b9, b10, b11] } }, "12");

    // With numeric floor '9', the only older entry is '7', which is terminal, so it is kept.
    // With lexicographical bug, floor is '10', so streaming '9' is classified as older, causing '7' to be dropped.
    const seqs = chat.timeline.entries.map((e) => e.seq);
    expect(seqs).toEqual(["7", "9", "10", "11"]);
  });

  it("attachments_on_live_queued_and_saving_inputs", () => {
    const attKey = "idem-att";
    const attRef: AttachmentRef = {
      attachment_id: "att-1",
      name: "doc.pdf",
      mime: "application/pdf",
      size: 1024,
      sha256: "abc",
      image: false,
    };
    const ops = {
      ops: new Map([
        [
          attKey,
          {
            key: attKey,
            kind: "input",
            conversationId: CONV,
            state: "saving",
            body: {
              content: [{ type: "text", text: "с файлом" }],
              attachment_ids: [attRef.attachment_id],
            },
            createdAt: 100,
            attempts: 0,
          },
        ],
      ]),
      settle() {},
      onOrphanRefused: () => () => undefined,
    };
    const chat = chatOf(ops as unknown as OpJournal);
    chat.open();
    const saving = chat.strip().find((i) => i.key === attKey);
    expect(saving).toBeDefined();
    expect(saving!.text).toBe("с файлом");
    expect(saving!.attachments.map((item) => item.attachment_id)).toEqual([
      attRef.attachment_id,
    ]);

    chat.applier.applyEvents([
      {
        event: ev(
          "attachment_state_changed",
          { ...attRef, state: "ready" },
          "2",
        ),
        overlap: false,
      },
    ]);

    chat.applier.applyEvents([
      {
        event: ev(
          "input_queued",
          {
            input_id: "inp-att",
            input_seq: "3",
            idempotency_key: attKey,
            state: "queued",
            content: [
              { type: "text", text: "с файлом" },
              { type: "attachment", attachment_id: "att-1" },
            ],
          },
          "3",
        ),
        overlap: false,
      },
    ]);

    const queued = chat.inputs.get(attKey);
    expect(queued).toBeDefined();
    expect(queued!.attachments).toHaveLength(1);
    expect(queued!.attachments[0]!.attachment_id).toBe("att-1");
    expect(queued!.attachments[0]!.name).toBe("doc.pdf");
  });

  it("newer_count_per_item_not_per_delta", () => {
    const chat = chatOf();
    chat.timeline.tailDetached = true;
    chat.timeline.newerCount = 0;

    for (let i = 1; i <= 10; i++) {
      chat.applier.applyEvents([
        { event: delta(i - 1, i, `chunk${i}`), overlap: false },
      ]);
    }
    expect(chat.timeline.newerCount).toBe(0);

    chat.applier.applyEvents([{ event: started(0, "b-new"), overlap: false }]);
    expect(chat.timeline.newerCount).toBe(1);
  });

  it("upsert_bounded_by_max_loaded", () => {
    const timeline = new TimelineModel();
    for (let i = 1; i <= 3000; i++) {
      timeline.upsert(entry(String(i)));
    }
    expect(timeline.entries).toHaveLength(3000);
    expect(timeline.oldestSeq()).toBe("1");
    expect(timeline.newestSeq()).toBe("3000");

    timeline.upsert(entry("3001"));
    expect(timeline.entries).toHaveLength(3000);
    expect(timeline.oldestSeq()).toBe("2");
    expect(timeline.newestSeq()).toBe("3001");
    expect(timeline.hasOlder).toBe(true);
  });

  it("window_text_flags_stale_and_low_remaining_correctly", () => {
    const now = Date.parse("2026-10-28T12:00:00.000Z");
    const freshWindow: LimitWindow = {
      observed_at: "2026-10-28T11:59:50.000Z",
      resets_at: "2026-10-28T15:00:00.000Z",
      used_permille: 150,
    };
    const view = windowText(freshWindow, now);
    expect(view.stale).toBe(false);
    expect(view.tone).toBe("normal");
    expect(view.text).toBe("85%");

    const lowWindow: LimitWindow = {
      observed_at: "2026-10-28T11:59:50.000Z",
      resets_at: "2026-10-28T15:00:00.000Z",
      used_permille: 850,
    };
    const lowView = windowText(lowWindow, now);
    expect(lowView.stale).toBe(false);
    expect(lowView.tone).toBe("warning");

    const staleWindow: LimitWindow = {
      observed_at: "2026-10-28T11:50:00.000Z",
      resets_at: "2026-10-28T15:00:00.000Z",
      used_permille: 150,
    };
    const staleView = windowText(staleWindow, now);
    expect(staleView.stale).toBe(true);
  });
});

function mulberry(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

function emptyCounts() {
  return {
    all: 0,
    pinned: 0,
    archived: 0,
    groups: [] as { id: string; count: number }[],
  };
}

function sampleRow(id: string): ListRow {
  return {
    id,
    title: "Сессия",
    title_source: "user",
    color: null,
    pinned: false,
    archived: false,
    groups: [],
    parent_id: null,
    spawn_depth: 0,
    machine: { id: "m", name: "dev" },
    harness: { id: "claude", title: "Claude", provider: "anthropic" },
    workdir: "/work",
    created_at: "2026-10-28T12:00:00.000Z",
    model: "opus",
    effort: "medium",
    permission_mode: "default",
    activity: { state: "idle" },
    unread_count: 0,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
    crunch: { enabled: false },
    last_activity_at: null,
  };
}

function queuedView(key: string, id: string) {
  return {
    id,
    idempotency_key: key,
    state: "queued",
    content: [{ type: "text" as const, text: key }],
    attachments: [],
    created_seq: "5",
    feed_seq: null,
    created_at: "2026-10-28T12:00:00.000Z",
    retry_of: null,
  };
}

function chatOf(
  ops?: OpJournal,
  clock: {
    now(): number;
    schedule?: (fn: () => void, ms: number) => () => void;
  } = { now: () => 0 },
  markRead?: ChatApi["markRead"],
  history?: ChatApi["history"],
): ChatStore {
  const api: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: history ?? (async () => ({ items: [] })),
    events: async () => ({ events: [], head: "0" }),
    markRead: markRead ?? (async () => undefined),
  };
  return new ChatStore(CONV, {
    api,
    ops:
      ops ??
      ({
        ops: new Map(),
        settle() {},
        onOrphanRefused: () => () => undefined,
      } as unknown as OpJournal),
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
      sha256Hex,
    },
    clock,
    probe: {},
  });
}

describe("stores/list deletion", () => {
  const row = (id: string): ListRow =>
    ({ id, title: id, archived: false, pinned: false, groups: [] }) as unknown as ListRow;
  const snapshot = (items: ListRow[], truncated = false) => ({
    counts: emptyCounts(),
    groups: [],
    items,
    limits: [],
    truncated,
    presets: [],
  });
  const seeded = () => {
    const list = listOf();
    list.applier.applySnapshot(snapshot([row("a"), row("b")]), "1");
    const gone: string[] = [];
    list.onDeleted((id) => gone.push(id));
    return { list, gone };
  };

  it("the_event_removes_the_row_updates_counts_and_tells_the_listeners", () => {
    const { list, gone } = seeded();
    const counts = { all: 1, pinned: 0, archived: 0, groups: [] };
    list.applier.applyEvents([
      {
        overlap: false,
        event: {
          kind: "conversation_deleted",
          payload: { id: "a", counts },
        } as unknown as HubEvent,
      },
    ]);
    expect([...list.items.keys()]).toEqual(["b"]);
    expect(list.counts).toEqual(counts);
    expect(list.deleted.has("a")).toBe(true);
    expect(gone).toEqual(["a"]);
    expect(list.unknownKinds).toBe(0);
  });

  it("a_client_offline_during_the_delete_learns_it_from_the_full_snapshot", () => {
    const { list, gone } = seeded();
    list.applier.applySnapshot(snapshot([row("b")]), "2");
    expect(gone).toEqual(["a"]);
  });

  it("a_truncated_snapshot_proves_nothing_about_missing_rows", () => {
    const { list, gone } = seeded();
    list.applier.applySnapshot(snapshot([row("b")], true), "2");
    expect(gone).toEqual([]);
    expect(list.deleted.size).toBe(0);
  });
});

function listOf(): ListStore {
  return new ListStore({
    connection: {} as ConnectionManager,
    api: {
      snapshot: async () => ({
        cursor: "0",
        data: {
          counts: emptyCounts(),
          groups: [],
          items: [],
          limits: [],
          truncated: false,
          presets: [],
        },
      }),
      events: async () => ({ events: [], head: "0" }),
    },
  });
}

function adapt(api: FakeHubApi): ChatApi {
  return {
    conversationSnapshot: (id) => api.conversationSnapshot(id),
    history: (id, query) => api.history(id, query),
    events: (scope, after, limit) => api.events(scope, after, limit),
    markRead: (id, seq) => api.markRead(id, seq),
  };
}
