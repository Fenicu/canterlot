import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import { freshInput } from "#lib/stores/inputs.js";
import { TimelineModel, type TimelineEntry } from "#lib/stores/timeline.js";
import { countEffectRuns } from "./effect-runs.svelte";

function block(id: string, seq: string): TimelineEntry {
  return {
    kind: "block",
    key: `block:${id}`,
    seq,
    block: new BlockModel(id, "text"),
  };
}

function input(id: string, seq: string): TimelineEntry {
  return {
    kind: "input",
    key: `input:${id}`,
    seq,
    input: freshInput(id, "текст", 0),
  };
}

describe("TimelineModel", () => {
  it("removing_an_absent_key_does_not_replace_the_entries", () => {
    const model = new TimelineModel();
    model.upsert(block("a", "1"));
    const before = model.entries;
    model.remove("input:нет");
    expect(model.entries).toBe(before);
  });

  it("upserting_the_same_entry_does_not_replace_the_entries", () => {
    const model = new TimelineModel();
    const first = input("a", "1");
    model.upsert(first);
    const before = model.entries;
    model.upsert({ ...first });
    expect(model.entries).toBe(before);
  });

  it("new_seq_or_new_object_for_a_key_replaces_the_entry_in_order", () => {
    const model = new TimelineModel();
    const moved = block("a", "1");
    model.upsert(moved);
    model.upsert(block("b", "2"));
    model.upsert({ ...moved, seq: "3" });
    expect(model.entries.map((entry) => entry.key)).toEqual(["block:b", "block:a"]);
    const fresh = block("b", "2");
    model.upsert(fresh);
    expect(model.entries.find((entry) => entry.key === "block:b")).toBe(fresh);
  });

  it("entries_stay_sorted_when_a_late_entry_lands_in_the_middle", () => {
    const model = new TimelineModel();
    for (const seq of ["10", "30", "20", "9", "100"]) model.upsert(block(seq, seq));
    expect(model.entries.map((entry) => entry.seq)).toEqual(["9", "10", "20", "30", "100"]);
  });

  it("detached_tail_counts_without_touching_the_entries", () => {
    const model = new TimelineModel();
    model.upsert(block("a", "1"));
    model.tailDetached = true;
    const before = model.entries;
    model.upsert(block("b", "2"));
    expect(model.entries).toBe(before);
    expect(model.newerCount).toBe(1);
  });
});

describe("TimelineModel: признак старых страниц", () => {
  it("hasOlder_is_reactive_so_a_derived_reader_sees_the_change", () => {
    const model = new TimelineModel();
    let seen = false;
    const watch = countEffectRuns(() => { seen = model.hasOlder; });
    flushSync();
    expect(watch.runs()).toBe(1);
    expect(seen).toBe(false);
    model.hasOlder = true;
    flushSync();
    expect(seen).toBe(true);
    watch.stop();
  });
});
