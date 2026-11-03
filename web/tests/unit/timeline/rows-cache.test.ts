import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import { RowsCache, buildRows } from "#lib/timeline/rows.js";

const NOW = Date.parse("2026-10-28T12:00:00.000Z");

function block(id: string, seq: string, run: string | null = null, parent: string | null = null): TimelineEntry {
  const model = new BlockModel(id, "text");
  model.inputCommandId = run;
  model.parentId = parent;
  model.createdSeq = seq;
  model.createdAt = NOW;
  model.text = id;
  return { kind: "block", key: `block:${id}`, seq, block: model };
}

function ids(rows: ReturnType<typeof buildRows>): string[] {
  return rows.flatMap((row) =>
    row.entries.map((entry) => (entry.kind === "block" ? entry.block.id : entry.key)),
  );
}

describe("incremental rows", () => {
  it("appends_a_tail_entry_without_rebuilding_prior_rows", () => {
    const cache = new RowsCache();
    const entries = Array.from({ length: 3000 }, (_, index) =>
      block(`b${index}`, String(index + 1)),
    );
    const first = cache.build(entries, { now: NOW });
    const next = cache.build([...entries, block("tail", "4000")], { now: NOW });

    expect(next.rows).toHaveLength(first.rows.length + 1);
    for (let index = 0; index < first.rows.length; index++)
      expect(next.rows[index]).toBe(first.rows[index]);
    expect(ids(next.rows).at(-1)).toBe("tail");
  });

  it("drops_the_head_row_without_redrawing_the_rest", () => {
    const cache = new RowsCache();
    const entries = Array.from({ length: 3000 }, (_, index) =>
      block(`b${index}`, String(index + 1)),
    );
    const first = cache.build(entries, { now: NOW });
    const next = cache.build([...entries.slice(1), block("tail", "4000")], { now: NOW });

    expect(next.rows).toHaveLength(first.rows.length);
    expect(next.rows[0]).toBe(first.rows[0]);
    expect(next.rows[1]).toBe(first.rows[2]);
    expect(ids(next.rows)).not.toContain("b0");
    expect(ids(next.rows)).toContain("b1");
    expect(ids(next.rows).at(-1)).toBe("tail");
  });

  it("matches_a_full_rebuild_through_appends_and_window_trims", () => {
    const cache = new RowsCache();
    const entries: TimelineEntry[] = [];
    for (let index = 0; index < 40; index++) {
      entries.push(block(`b${index}`, String(index + 1), index % 3 === 0 ? "run-1" : null));
      const cached = cache.build([...entries], { now: NOW });
      const full = buildRows(entries, { now: NOW });
      expect(cached.rows.map((row) => row.key)).toEqual(full.map((row) => row.key));
      expect(ids(cached.rows)).toEqual(ids(full));
    }
    for (let index = 40; index < 60; index++) {
      entries.push(block(`b${index}`, String(index + 1), null));
      if (entries.length > 30) entries.shift();
      const cached = cache.build([...entries], { now: NOW });
      const full = buildRows(entries, { now: NOW });
      // Ключи переживают сдвиг окна, поэтому сверяем вид и состав строк.
      expect(cached.rows.map((row) => row.kind)).toEqual(full.map((row) => row.kind));
      expect(cached.rows.map((row) => row.label ?? null)).toEqual(
        full.map((row) => row.label ?? null),
      );
      expect(ids(cached.rows)).toEqual(ids(full));
    }
  });

  it("keeps_children_out_of_rows_but_updates_links_for_the_parent_card", () => {
    const cache = new RowsCache();
    const root = block("root", "1", "run-1");
    const first = cache.build([root], { now: NOW });
    const child = block("child", "2", "run-1", "root");
    const next = cache.build([root, child], { now: NOW });
    expect(next.rows).toBe(first.rows);
    expect(next.links.children("root").map((item) => item.id)).toEqual(["child"]);
  });

  it("rebuilds_when_the_day_changes", () => {
    const cache = new RowsCache();
    const entries = [block("a", "1")];
    const first = cache.build(entries, { now: NOW });
    const next = cache.build(entries, { now: NOW + 24 * 3_600_000 });
    expect(next.rows).not.toBe(first.rows);
    expect(next.rows[0]?.kind).toBe("day");
  });
});
