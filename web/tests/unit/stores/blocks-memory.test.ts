import { describe, expect, it } from "vitest";
import {
  BlockModel,
  TOOL_TAIL_BYTES,
  applyBlockEvent,
  pruneBlocks,
  trimFinalBody,
  utf8Length,
} from "#lib/stores/blocks.js";
import { TimelineModel } from "#lib/stores/timeline.js";
import type { HubEvent } from "#lib/sync/wire.js";

const BLOCK = "00000000-0000-4000-8000-0000000000b1";
const CONV = "00000000-0000-4000-8000-0000000000aa";

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

function toolBlock(text: string, state: "final" | "streaming" = "final"): BlockModel {
  const block = new BlockModel(BLOCK, "tool_result");
  block.body = text;
  block.bytes = utf8Length(text);
  block.text = text;
  block.lineCount = text.split("\n").length;
  block.state = state;
  return block;
}

describe("block memory", () => {
  it("keeps_only_a_bounded_tail_of_a_final_tool_body", () => {
    const body = `${"строка\n".repeat(20_000)}ХВОСТ`;
    const block = toolBlock(body);
    const lines = block.lineCount;
    trimFinalBody(block);

    expect(utf8Length(block.body)).toBeLessThanOrEqual(TOOL_TAIL_BYTES);
    expect(block.body.endsWith("ХВОСТ")).toBe(true);
    expect(block.lineCount).toBe(lines);
    expect(block.bytes).toBe(utf8Length(body));
    expect(block.truncated).toBe(true);
  });

  it("does_not_cut_a_live_tool_body_or_a_final_answer", () => {
    const live = toolBlock("x".repeat(TOOL_TAIL_BYTES * 2), "streaming");
    trimFinalBody(live);
    expect(live.body).toHaveLength(TOOL_TAIL_BYTES * 2);

    const answer = new BlockModel("answer", "text");
    answer.body = "x".repeat(TOOL_TAIL_BYTES * 2);
    answer.bytes = utf8Length(answer.body);
    answer.text = answer.body;
    answer.state = "final";
    trimFinalBody(answer);
    expect(answer.body).toHaveLength(TOOL_TAIL_BYTES * 2);
  });

  it("trims_a_final_block_after_the_hub_sends_it", () => {
    const blocks = new Map<string, BlockModel>();
    const text = `${"y".repeat(TOOL_TAIL_BYTES * 2)}конец`;
    const started = ev(
      "block_started",
      {
        block_id: BLOCK,
        rev: 0,
        kind: "tool_result",
        input_command_id: null,
        parent_block_id: null,
        agent: "agent",
        model: "opus",
        tool_name: "Bash",
        summary: null,
        approved_by: null,
      },
      "1",
    );
    const final = ev(
      "block_final",
      {
        block_id: BLOCK,
        rev: 1,
        text,
        checksum: "00",
        text_omitted: false,
        summary: null,
      },
      "2",
    );
    applyBlockEvent(blocks, started);
    applyBlockEvent(blocks, final);
    expect(blocks.get(BLOCK)!.body.length).toBeLessThan(text.length);
    expect(blocks.get(BLOCK)!.text.endsWith("конец")).toBe(true);
  });

  it("drops_blocks_the_window_no_longer_references_but_keeps_their_children", () => {
    const blocks = new Map<string, BlockModel>();
    const main = new BlockModel("main", "text");
    const child = new BlockModel("child", "tool_result");
    child.parentId = "main";
    const grand = new BlockModel("grand", "tool_result");
    grand.parentId = "child";
    const gone = new BlockModel("gone", "tool_result");
    for (const block of [main, child, grand, gone]) blocks.set(block.id, block);

    expect(pruneBlocks(blocks, ["main"])).toBe(1);
    expect([...blocks.keys()]).toEqual(["main", "child", "grand"]);
  });

  it("asks_the_chat_to_prune_when_the_timeline_drops_old_entries", () => {
    let trims = 0;
    const timeline = new TimelineModel(() => {
      trims += 1;
    });
    const entry = (seq: string) => {
      const block = new BlockModel(`b${seq}`, "text");
      block.createdSeq = seq;
      return { kind: "block" as const, key: `block:b${seq}`, seq, block };
    };
    for (let index = 1; index <= 3001; index++) timeline.upsert(entry(String(index)));
    expect(trims).toBe(1);
  });
});
