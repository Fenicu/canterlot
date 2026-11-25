import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import type { CompactionModel, TimelineEntry } from "#lib/stores/timeline.js";
import { buildRows, currentRun, nonLastRunCards, runSlotCommand, settledRunCards } from "#lib/timeline/rows.js";

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

describe("runSlotCommand", () => {
  // Слот называет ввод (`inputs.id`), блоки хода несут его команду: id не совпадают.
  const INPUT = "0198f1a0-0000-7000-8000-00000000a001";
  const COMMAND = "0198f1a0-0000-7000-8000-00000000c001";

  it("names_the_command_of_a_running_or_dispatching_slot_never_its_input", () => {
    expect(runSlotCommand({ running: { input_id: INPUT, command_id: COMMAND } })).toEqual({ command: COMMAND });
    expect(runSlotCommand({ dispatching: { input_id: INPUT, attempt: 1, command_id: COMMAND } })).toEqual({ command: COMMAND });
  });

  it("knows_a_slot_is_taken_but_not_its_turn_when_an_old_hub_sent_no_command", () => {
    expect(runSlotCommand({ running: { input_id: INPUT } })).toEqual({ command: null });
  });

  it("returns_nothing_for_a_free_or_missing_slot", () => {
    expect(runSlotCommand("free")).toBeNull();
    expect(runSlotCommand(undefined)).toBeNull();
    expect(runSlotCommand(null)).toBeNull();
  });

  it("settles_the_earlier_card_of_the_running_turn_by_the_slot_command", () => {
    const rows = buildRows([block("a", "1", COMMAND, "streaming"), compaction("c", "2"), block("b", "3", COMMAND)], { now: NOW });
    const cards = rows.filter((row) => row.kind === "answer");
    const slot = { running: { input_id: INPUT, command_id: COMMAND } };
    expect([...settledRunCards(rows, runSlotCommand(slot)!.command)]).toEqual([cards[0]!.key]);
    // Id ввода с блоками не совпадает: по нему карточка не успокоилась бы никогда.
    expect(settledRunCards(rows, INPUT).size).toBe(0);
  });
});

describe("currentRun", () => {
  const slot = { command: "c-slot" };

  it("takes_the_slot_command_whenever_the_slot_is_taken", () => {
    expect(currentRun("c-old", { live: true, slot, latest: "c-other" })).toBe("c-slot");
  });

  it("keeps_a_cli_turn_while_no_block_is_newer_than_since", () => {
    expect(currentRun(null, { live: true, slot: null, latest: "c1" })).toBe("c1");
    expect(currentRun("c1", { live: true, slot: null, latest: null })).toBe("c1");
  });

  it("moves_to_a_newer_turn_and_forgets_at_the_end_of_the_turn", () => {
    expect(currentRun("c1", { live: true, slot: null, latest: "c2" })).toBe("c2");
    expect(currentRun("c2", { live: false, slot: null, latest: "c2" })).toBeNull();
    expect(currentRun(null, { live: true, slot: null, latest: null })).toBeNull();
  });
});
