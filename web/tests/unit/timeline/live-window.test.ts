import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import type { RequestItem } from "#lib/stores/requests.svelte.js";
import { blockLinks } from "#lib/timeline/rows.js";
import { liveWindow, type LiveWindowContext, type WindowItem } from "#lib/timeline/liveWindow.js";

let seq = 0;

function block(
  kind: BlockModel["kind"],
  opts: { tool?: string; text?: string; parent?: string; state?: BlockModel["state"]; summary?: string; agent?: string } = {},
): TimelineEntry {
  seq += 1;
  const model = new BlockModel(`b${seq}`, kind);
  model.toolName = opts.tool ?? null;
  model.parentId = opts.parent ?? null;
  model.summary = opts.summary ?? null;
  model.agent = opts.agent ?? "";
  model.text = opts.text ?? "";
  model.state = opts.state ?? "final";
  model.inputCommandId = "run";
  model.createdAt = seq * 1000;
  model.startedAt = seq * 1000;
  model.finishedAt = seq * 1000;
  return { kind: "block", key: `block:${model.id}`, seq: String(seq), block: model };
}

const idOf = (entry: TimelineEntry): string => (entry.kind === "block" ? entry.block.id : "");

function call(tool = "Bash", opts: { text?: string; summary?: string; state?: BlockModel["state"]; agent?: string } = {}): TimelineEntry {
  return block("tool_call", { tool, ...opts });
}

function result(callEntry: TimelineEntry, opts: { text?: string; summary?: string } = {}): TimelineEntry {
  return block("tool_result", { parent: idOf(callEntry), ...opts });
}

function request(id: string): TimelineEntry {
  const item: RequestItem = { id, conversationId: "c", kind: "permission", state: "applied", payload: {} };
  return { kind: "request", key: `request:${id}`, seq: String(1000 + Number(id.slice(1))), request: item };
}

function split(entries: TimelineEntry[], over: Partial<LiveWindowContext> = {}) {
  return liveWindow(entries, { links: blockLinks(entries), working: false, ...over });
}

const kinds = (items: WindowItem[]) => items.map((item) => item.kind);

describe("liveWindow", () => {
  it("keeps_the_intro_before_the_head_and_orders_the_stream", () => {
    const intro = block("text", { text: "I'll start by reading the brief." });
    const reasoning = block("reasoning", { text: "Сначала посмотрю бриф", state: "streaming" });
    const step = call();
    const stepResult = result(step, { text: "ok" });
    const after = block("text", { text: "Теперь правлю" });
    const view = split([intro, reasoning, step, stepResult, after]);

    expect(view.before).toEqual([intro]);
    expect(kinds(view.stream)).toEqual(["reasoning", "tool", "text"]);
    expect(view.stream.map((item) => item.key)).toEqual([reasoning.key, step.key, after.key]);
    expect(view.requests).toEqual([]);
    expect(view.head.count).toBe(1);
  });

  it("leaves_a_turn_without_steps_as_it_is", () => {
    const intro = block("text", { text: "Просто ответ" });
    const view = split([intro, request("r1")]);

    expect(view.before).toEqual([intro]);
    expect(view.stream).toEqual([]);
    expect(view.requests).toHaveLength(1);
    expect(view.head).toEqual({ count: 0, tally: "", failed: 0, startedAt: 0 });
  });

  it("puts_text_after_the_first_step_into_the_stream", () => {
    const intro = block("text");
    const step = call();
    const after = block("text", { text: "промежуточный итог" });
    const view = split([intro, step, after]);

    expect(view.before).toEqual([intro]);
    expect(view.stream.map((item) => (item.kind === "text" ? item.block.text : item.kind))).toEqual(["tool", "промежуточный итог"]);
  });

  it("extracts_requests_from_before_and_from_the_stream", () => {
    const first = request("r1");
    const intro = block("text");
    const step = call();
    const second = request("r2");
    const view = split([first, intro, step, second]);

    expect(view.requests.map((entry) => entry.key)).toEqual([first.key, second.key]);
    expect(view.before).toEqual([intro]);
    expect(kinds(view.stream)).toEqual(["tool"]);
  });

  it("merges_a_result_into_its_call_and_keeps_an_orphan_result", () => {
    const step = call();
    const stepResult = result(step, { text: "вывод вызова" });
    const orphan = block("tool_result", { parent: "missing", text: "вывод без вызова" });
    const view = split([step, stepResult, orphan]);

    expect(kinds(view.stream)).toEqual(["tool", "result"]);
    const tool = view.stream[0];
    expect(tool?.kind === "tool" && tool.result).toBe(stepResult.kind === "block" ? stepResult.block : null);
    expect(tool?.kind === "tool" && tool.detail).toBe("");
    expect(view.stream[1]?.kind === "result" && view.stream[1].block.text).toBe("вывод без вызова");
    expect(view.head.count).toBe(2);
  });

  it("caps_the_stream_at_twelve_while_the_head_counts_every_call", () => {
    const calls = Array.from({ length: 15 }, () => call());
    const view = split(calls);

    expect(view.stream).toHaveLength(12);
    expect(view.stream[0]?.key).toBe(calls[3]?.key);
    expect(view.stream.at(-1)?.key).toBe(calls[14]?.key);
    expect(view.head.count).toBe(15);
    expect(view.head.tally).toBe("Bash 15");
    expect(view.head.startedAt).toBe(calls[0]?.kind === "block" ? calls[0].block.createdAt : 0);
  });

  it("counts_failed_calls_on_the_head_and_on_their_line", () => {
    const good = call();
    const bad = call();
    const badResult = result(bad, { summary: "error", text: "error: нет файла" });
    const view = split([good, result(good), bad, badResult]);

    expect(view.head.failed).toBe(1);
    const failed = view.stream.find((item) => item.kind === "tool" && item.block.id === idOf(bad));
    expect(failed?.kind === "tool" && failed.failed).toBe(true);
  });

  it("keeps_quiet_and_subagent_calls_as_tool_lines_with_their_names", () => {
    const quiet = call("ToolSearch");
    const agent = call("Agent", { summary: "прочитать бриф" });
    const bash = call("Bash");
    const view = split([quiet, agent, bash], {
      label: (model) => (model.toolName === "Bash" ? "Команда" : null),
    });

    const names = view.stream.map((item) => (item.kind === "tool" ? item.name : item.kind));
    expect(names).toEqual(["ToolSearch", "Субагент", "Команда"]);
    const subagent = view.stream[1];
    expect(subagent?.kind === "tool" && subagent.detail).toBe("прочитать бриф");
    expect(view.head.tally).toBe("ToolSearch 1 · Субагент 1 · Команда 1");
  });

  it("starts_the_clock_at_the_first_step", () => {
    const intro = block("text");
    const reasoning = block("reasoning", { text: "думаю" });
    const step = call();
    const view = split([intro, reasoning, step]);

    expect(view.head.startedAt).toBe(reasoning.kind === "block" ? reasoning.block.createdAt : 0);
  });
});
