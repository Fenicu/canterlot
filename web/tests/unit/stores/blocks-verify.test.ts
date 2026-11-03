import { describe, expect, it } from "vitest";
import {
  BlockModel,
  BlockSync,
  applyBlockEvent,
  sha256Hex,
  type BlockIO,
} from "#lib/stores/blocks.js";
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

function started(): HubEvent {
  return ev(
    "block_started",
    {
      block_id: BLOCK,
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
    "1",
  );
}

function delta(base: number, rev: number, append: string): HubEvent {
  return ev(
    "block_delta",
    { block_id: BLOCK, base_rev: base, rev, append },
    String(rev),
  );
}

function final(text: string, checksum: string, omitted: boolean): HubEvent {
  return ev(
    "block_final",
    {
      block_id: BLOCK,
      rev: 2,
      text,
      checksum,
      text_omitted: omitted,
      summary: null,
    },
    "2",
  );
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

describe("block verification", () => {
  it("does_not_hash_the_body_the_hub_already_sent", async () => {
    const blocks = new Map<string, BlockModel>();
    let hashes = 0;
    await drive(
      blocks,
      [started(), delta(0, 1, "черновик"), final("черновик", "00", false)],
      {
        sha256Hex: () => {
          hashes++;
          return Promise.resolve("00");
        },
        fetchContent: () => {
          throw new Error("no_refetch");
        },
      },
    );
    expect(hashes).toBe(0);
    expect(blocks.get(BLOCK)!.state).toBe("final");
    expect(blocks.get(BLOCK)!.text).toBe("черновик");
  });

  it("late_incomplete_does_not_downgrade_a_final_block", async () => {
    const blocks = new Map<string, BlockModel>();
    await drive(
      blocks,
      [
        started(),
        delta(0, 1, "готово"),
        final("готово", "00", false),
        ev(
          "block_incomplete",
          { block_id: BLOCK, rev: 2, reason: "interrupted" },
          "3",
        ),
      ],
      {
        sha256Hex: () => Promise.resolve("00"),
        fetchContent: () => {
          throw new Error("no_refetch");
        },
      },
    );
    expect(blocks.get(BLOCK)!.state).toBe("final");
  });

  it("still_verifies_the_accumulated_body_when_the_hub_omitted_the_text", async () => {
    const blocks = new Map<string, BlockModel>();
    const text = "накоплено";
    const checksum = await sha256Hex(text);
    await drive(
      blocks,
      [started(), delta(0, 1, text), final("", checksum, true)],
      {
        sha256Hex,
        fetchContent: () => {
          throw new Error("no_refetch");
        },
      },
    );
    expect(blocks.get(BLOCK)!.state).toBe("final");
    expect(blocks.get(BLOCK)!.text).toBe(text);
  });
});
