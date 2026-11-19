import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import type { CompactionModel, TimelineEntry } from "#lib/stores/timeline.js";
import { buildRows, nonLastRunCards, settledRunCards } from "#lib/timeline/rows.js";

const NOW = Date.parse("2026-10-28T12:00:00.000Z");

function block(id: string, seq: string, run: string, state: "streaming" | "final" = "final"): TimelineEntry {
  const model = new BlockModel(id, "text");
  model.inputCommandId = run;
  model.createdSeq = seq;
  model.createdAt = NOW;
  model.state = state;
  model.text = id;
  return { kind: "block", key: `block:${id}`, seq, block: model };
}

function compaction(id: string, seq: string): TimelineEntry {
  const model: CompactionModel = { id, trigger: "auto", preTokens: null, ok: true, createdAt: NOW, finishedAt: NOW };
  return { kind: "compaction", key: `compaction:${id}`, seq, compaction: model };
}

const answers = (entries: TimelineEntry[]) => buildRows(entries, { now: NOW }).filter((row) => row.kind === "answer");

describe("settledRunCards", () => {
  it("settles_every_card_of_the_active_run_but_the_last", () => {
    const rows = buildRows([block("a", "1", "r1", "streaming"), compaction("c", "2"), block("b", "3", "r1", "streaming")], { now: NOW });
    const cards = rows.filter((row) => row.kind === "answer");
    expect(cards).toHaveLength(2);
    const settled = settledRunCards(rows, "r1");
    expect([...settled]).toEqual([cards[0]!.key]);
  });

  it("settles_nothing_for_a_single_card_or_without_an_active_run", () => {
    const one = buildRows([block("a", "1", "r1")], { now: NOW });
    expect(settledRunCards(one, "r1").size).toBe(0);
    const split = buildRows([block("a", "1", "r1"), compaction("c", "2"), block("b", "3", "r1")], { now: NOW });
    expect(settledRunCards(split, null).size).toBe(0);
  });

  it("leaves_cards_of_other_runs_alone", () => {
    const entries = [block("a", "1", "r0"), compaction("c", "2"), block("b", "3", "r1")];
    const rows = buildRows(entries, { now: NOW });
    expect(answers(entries)).toHaveLength(2);
    expect(settledRunCards(rows, "r1").size).toBe(0);
  });

  it("keeps_the_active_run_subset_of_every_earlier_card", () => {
    const rows = buildRows([block("a", "1", "r0"), compaction("c", "2"), block("b", "3", "r0"), block("d", "4", "r1", "streaming")], { now: NOW });
    const cards = rows.filter((row) => row.kind === "answer");
    expect(cards).toHaveLength(3);
    expect([...nonLastRunCards(rows).keys()]).toEqual([cards[0]!.key]);
    expect(settledRunCards(rows, "r1").size).toBe(0);
    expect([...settledRunCards(rows, "r0")]).toEqual([cards[0]!.key]);
  });
});

describe("nonLastRunCards", () => {
  it("names_only_the_first_card_of_the_split_run_among_two_runs", () => {
    const rows = buildRows(
      [block("a", "1", "r1"), compaction("c", "2"), block("b", "3", "r1"), block("d", "4", "r2")],
      { now: NOW },
    );
    const cards = rows.filter((row) => row.kind === "answer");
    expect(cards).toHaveLength(3);
    expect([...nonLastRunCards(rows).keys()]).toEqual([cards[0]!.key]);
  });

  it("never_names_cards_without_a_run_id", () => {
    const rows = buildRows([block("a", "1", ""), compaction("c", "2"), block("b", "3", "")], { now: NOW });
    expect(nonLastRunCards(rows).size).toBe(0);
  });
});
