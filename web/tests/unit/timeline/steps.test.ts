import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import { FeedFolds } from "#lib/timeline/folds.js";
import { blockLinks } from "#lib/timeline/rows.js";
import { foldSteps, type CardItem, type FoldContext } from "#lib/timeline/steps.js";

let seq = 0;

function entry(kind: BlockModel["kind"], opts: { tool?: string; parent?: string; state?: BlockModel["state"]; summary?: string } = {}): TimelineEntry {
  seq += 1;
  const block = new BlockModel(`b${seq}`, kind);
  block.toolName = opts.tool ?? null;
  block.parentId = opts.parent ?? null;
  block.summary = opts.summary ?? null;
  block.state = opts.state ?? "final";
  block.inputCommandId = "run";
  block.createdAt = seq * 1000;
  block.finishedAt = seq * 1000;
  return { kind: "block", key: `block:${block.id}`, seq: String(seq), block };
}

function step(tool = "Bash", failed = false): TimelineEntry[] {
  const call = entry("tool_call", { tool });
  const id = call.kind === "block" ? call.block.id : "";
  return [call, entry("tool_result", { parent: id, summary: failed ? "error" : undefined })];
}

const top = (list: TimelineEntry[]) => list.filter((item) => item.kind !== "block" || item.block.parentId === null);

function fold(list: TimelineEntry[], over: Partial<FoldContext> = {}, folds = new FeedFolds()): CardItem[] {
  return foldSteps(top(list), { links: blockLinks(list), working: false, live: false, folds, ...over });
}

const shape = (items: CardItem[]) => items.map((item) => (item.kind === "group" ? `group:${item.label}` : item.entry.kind === "block" ? item.entry.block.kind : item.entry.kind));

describe("step groups", () => {
  it("folds_three_steps_with_their_reasoning_and_leaves_text_outside", () => {
    const list = [entry("text"), ...step(), entry("reasoning"), ...step("Read"), ...step(), entry("text")];
    const items = fold(list);
    expect(shape(items)).toEqual(["text", "group:3 шага", "text"]);
    const group = items[1];
    expect(group?.kind === "group" && group.tally).toBe("Bash 2 · Read 1");
    expect(group?.kind === "group" && group.entries).toHaveLength(4);
  });

  it("keeps_short_runs_as_they_are", () => {
    expect(shape(fold([...step(), entry("reasoning"), ...step(), entry("text")]))).toEqual(["tool_call", "reasoning", "tool_call", "text"]);
  });

  it("text_and_notices_split_a_run", () => {
    const list = [...step(), ...step(), entry("warning"), ...step(), ...step()];
    expect(shape(fold(list))).toEqual(["tool_call", "tool_call", "warning", "tool_call", "tool_call"]);
  });

  it("a_failed_step_is_counted_on_the_summary", () => {
    const items = fold([...step(), ...step("Bash", true), ...step()]);
    expect(items[0]?.kind === "group" && items[0].failed).toBe(1);
  });

  it("a_running_step_stays_visible_and_counts_towards_the_group", () => {
    const running = entry("tool_call", { tool: "Bash", state: "streaming" });
    const items = fold([...step(), ...step(), running], { live: true, working: true });
    expect(shape(items)).toEqual(["group:2 шага", "tool_call"]);
    expect(items[1]?.key).toBe(running.key);
  });

  it("the_last_finished_step_stays_outside_while_the_turn_runs_and_joins_after", () => {
    const list = [...step(), ...step(), ...step()];
    expect(shape(fold(list, { live: true, working: true }))).toEqual(["group:2 шага", "tool_call"]);
    expect(shape(fold(list))).toEqual(["group:3 шага"]);
  });

  it("group_keeps_its_name_when_it_grows_from_both_ends", () => {
    const folds = new FeedFolds();
    const older = step();
    const middle = [...step(), ...step(), ...step()];
    const newer = step();
    const first = fold(middle, {}, folds)[0]?.key;
    expect(fold([...older, ...middle], {}, folds)[0]?.key).toBe(first);
    expect(fold([...older, ...middle, ...newer], {}, folds)[0]?.key).toBe(first);
  });

  it("a_split_group_gets_two_names", () => {
    const folds = new FeedFolds();
    const left = [...step(), ...step(), ...step()];
    const right = [...step(), ...step(), ...step()];
    fold([...left, ...right], {}, folds);
    const items = fold([...left, entry("text"), ...right], {}, folds);
    expect(new Set(items.map((item) => item.key)).size).toBe(3);
  });
});

describe("cut", () => {
  const rows = (count: number, from = 0) => Array.from({ length: count }, (_, index) => ({ key: `k${from + index}` }));

  it("is_decided_once_and_rows_added_later_stay_visible", () => {
    const folds = new FeedFolds();
    const first = folds.view("card", rows(70, 100), false);
    expect(first.hidden).toBe(30);
    expect(first.seam).toBe("k150");
    const grown = folds.view("card", [...rows(30, 70), ...rows(70, 100), ...rows(5, 170)], false);
    expect(grown.hidden).toBe(30);
    expect(grown.shown.map((row) => row.key)).toContain("k70");
    expect(grown.shown.map((row) => row.key)).toContain("k174");
  });

  it("never_cuts_a_list_first_seen_short_or_live", () => {
    const folds = new FeedFolds();
    folds.view("short", rows(40), false);
    expect(folds.view("short", rows(200), false).hidden).toBe(0);
    folds.view("live", rows(70), true);
    expect(folds.view("live", rows(70), false).hidden).toBe(0);
  });

  it("is_gone_after_reveal", () => {
    const folds = new FeedFolds();
    folds.view("card", rows(70), false);
    folds.reveal("card");
    expect(folds.view("card", rows(70), false).hidden).toBe(0);
  });
});
