import { describe, expect, it, vi } from "vitest";
import { BlockModel, applyBlockEvent } from "#lib/stores/blocks.js";
import type { HubEvent } from "#lib/sync/wire.js";

const BLOCK = "00000000-0000-4000-8000-0000000000b1";
const CONV = "00000000-0000-4000-8000-0000000000aa";

function delta(base: number, rev: number, append: string): HubEvent {
  return {
    v: 1,
    id: "00000000-0000-4000-8000-0000000000ee",
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: `conversation:${CONV}`,
    scope_seq: String(rev),
    scope_seq_from: String(rev),
    committed_at: "2026-10-28T12:00:00.000Z",
    committed_at_from: "2026-10-28T12:00:00.000Z",
    conversation_id: CONV,
    execution_id: null,
    kind: "block_delta",
    payload: { block_id: BLOCK, base_rev: base, rev, append },
  } as HubEvent;
}

function streamingBlock(): { blocks: Map<string, BlockModel>; block: BlockModel } {
  const block = new BlockModel(BLOCK, "text");
  block.body = "начало";
  block.bytes = 12;
  block.text = "начало";
  return { blocks: new Map([[BLOCK, block]]), block };
}

describe("line count on delta", () => {
  it("does_not_rescan_the_whole_body_for_every_append", () => {
    const { blocks, block } = streamingBlock();
    const split = vi.spyOn(String.prototype, "split");
    let calls = 0;
    try {
      applyBlockEvent(blocks, delta(0, 1, "\nвторой"));
      applyBlockEvent(blocks, delta(1, 2, "\nтретий"));
      calls = split.mock.calls.length;
    } finally {
      split.mockRestore();
    }
    expect(calls).toBe(0);
    expect(block.lineCount).toBe(3);
    expect(block.text).toBe("начало\nвторой\nтретий");
  });

  it("keeps_the_trailing_newline_rule_for_incremental_appends", () => {
    const { blocks, block } = streamingBlock();
    applyBlockEvent(blocks, delta(0, 1, "\n"));
    expect(block.lineCount).toBe(1);
    applyBlockEvent(blocks, delta(1, 2, "второй\n"));
    expect(block.lineCount).toBe(2);
    applyBlockEvent(blocks, delta(2, 3, "третий"));
    expect(block.lineCount).toBe(3);
    applyBlockEvent(blocks, delta(3, 4, "\n"));
    expect(block.lineCount).toBe(3);
  });
});
