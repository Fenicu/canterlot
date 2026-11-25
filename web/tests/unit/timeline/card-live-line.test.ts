import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import { cardLiveLine, type LiveTurnLine } from "#lib/timeline/present.js";

const NOW = Date.parse("2026-11-25T10:00:00.000Z");

function block(id: string, kind: BlockModel["kind"], opts: { state?: "streaming" | "final"; tool?: string; parent?: string; at?: number } = {}): BlockModel {
  const model = new BlockModel(id, kind);
  model.state = opts.state ?? "final";
  model.toolName = opts.tool ?? null;
  model.parentId = opts.parent ?? null;
  model.createdAt = opts.at ?? NOW - 60_000;
  model.startedAt = model.createdAt;
  return model;
}

const noResults = { result: () => null };
const tool: LiveTurnLine = { phase: "tool", tool: "cargo test", startedAt: NOW - 30_000 };
const generating: LiveTurnLine = { phase: "working", tool: "", startedAt: NOW - 20_000 };

describe("cardLiveLine", () => {
  it("takes_the_hub_activity_over_the_blocks_in_the_main_feed", () => {
    const blocks = [block("t", "text"), block("c", "tool_call", { tool: "Bash" })];
    expect(cardLiveLine({ blocks, links: noResults, turn: tool, lane: false, now: NOW })).toEqual(tool);
  });

  it("refines_generating_into_thinking_only_for_streaming_reasoning", () => {
    const reasoning = block("r", "reasoning", { state: "streaming", at: NOW - 5_000 });
    expect(cardLiveLine({ blocks: [reasoning], links: noResults, turn: generating, lane: false, now: NOW })).toEqual({ phase: "thinking", tool: "", startedAt: NOW - 5_000 });
    // Модель ещё пишет вызов: шапка говорит «Генерирует», и строка карточки тоже.
    const call = block("c", "tool_call", { state: "streaming", tool: "Bash" });
    expect(cardLiveLine({ blocks: [call], links: noResults, turn: generating, lane: false, now: NOW })).toEqual(generating);
    // Хаб уже знает инструмент: зависшее рассуждение не перебивает его.
    expect(cardLiveLine({ blocks: [reasoning], links: noResults, turn: tool, lane: false, now: NOW })).toEqual(tool);
  });

  it("names_the_open_call_at_the_end_of_a_lane_card", () => {
    const call = block("c", "tool_call", { tool: "Bash", parent: "lane", at: NOW - 7_000 });
    const line = cardLiveLine({ blocks: [block("t", "text", { parent: "lane" }), call], links: noResults, turn: null, lane: true, now: NOW });
    expect(line.phase).toBe("tool");
    expect(line.tool).not.toBe("");
    expect(line.startedAt).toBe(NOW - 7_000);
  });

  it("does_not_guess_a_tool_from_an_open_call_in_the_main_feed_or_after_its_result", () => {
    const call = block("c", "tool_call", { tool: "Bash" });
    // В основной ленте вызов без результата бывает ожиданием разрешения.
    expect(cardLiveLine({ blocks: [call], links: noResults, turn: null, lane: false, now: NOW }).phase).toBe("working");
    const done = { result: (id: string) => (id === "c" ? block("res", "tool_result", { parent: "c" }) : null) };
    expect(cardLiveLine({ blocks: [call], links: done, turn: null, lane: true, now: NOW }).phase).toBe("working");
  });

  it("keeps_the_streaming_block_phase_without_activity", () => {
    const reasoning = block("r", "reasoning", { state: "streaming", at: NOW - 3_000 });
    expect(cardLiveLine({ blocks: [block("t", "text"), reasoning], links: noResults, turn: null, lane: true, now: NOW })).toMatchObject({ phase: "thinking", startedAt: NOW - 3_000 });
  });
});
