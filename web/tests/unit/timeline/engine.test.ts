import { describe, expect, it } from "vitest";
import { BlockModel, utf8Length } from "#lib/stores/blocks.js";
import { freshInput, type InputUiState } from "#lib/stores/inputs.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import {
  HeightIndex,
  captureAnchor,
  restoreAnchor,
  visibleRange,
} from "#lib/timeline/virtual.js";
import { buildRows } from "#lib/timeline/rows.js";
import {
  TOOL_DOM_CAP,
  TOOL_PAGE_BYTES,
  activityLiveLine,
  liveLineText,
  takeToolPage,
  toolStatusText,
} from "#lib/timeline/present.js";

describe("timeline window", () => {
  it("height_index_prefix_sums_stay_consistent_under_random_updates", () => {
    const ix = new HeightIndex(() => 10);
    const naive = Array.from({ length: 200 }, () => 10);
    ix.setCount(naive.length);
    const random = mulberry(7);
    for (let step = 0; step < 400; step++) {
      const index = Math.floor(random() * naive.length);
      const height = 1 + Math.floor(random() * 90);
      ix.set(index, height);
      naive[index] = height;
      const at = Math.floor(random() * (naive.length + 1));
      let sum = 0;
      for (let i = 0; i < at; i++) sum += naive[i] ?? 0;
      expect(ix.offset(at)).toBe(sum);
    }
    expect(ix.total()).toBe(naive.reduce((sum, height) => sum + height, 0));
  });

  it("visible_range_for_10000_rows_is_bounded_and_pads_sum_to_total", () => {
    const ix = new HeightIndex(() => 40);
    ix.setCount(10_000);
    const range = visibleRange(ix, 2_000, 800, 1_200);
    expect(range.end - range.start).toBeGreaterThan(0);
    expect(range.end - range.start).toBeLessThan(200);
    let shown = 0;
    for (let i = range.start; i < range.end; i++) shown += 40;
    expect(range.padTop + shown + range.padBottom).toBe(ix.total());
  });

  it("anchor_restore_keeps_the_offset_after_prepend_and_height_changes_above", () => {
    const ix = new HeightIndex(() => 10);
    ix.setCount(20);
    const keys = Array.from({ length: 20 }, (_, index) => `k${index}`);
    const scrollTop = ix.offset(5) + 3;
    const anchor = captureAnchor(ix, keys, scrollTop);
    expect(anchor).toEqual({ key: "k5", offsetWithin: 3 });
    const prepended = 4;
    const next = new HeightIndex(() => 10);
    next.setCount(prepended + keys.length);
    const nextKeys = [
      ...Array.from({ length: prepended }, (_, index) => `p${index}`),
      ...keys,
    ];
    const shifted = restoreAnchor(next, nextKeys, anchor!);
    expect(shifted - scrollTop).toBe(prepended * 10);
    next.set(1, 25);
    expect(restoreAnchor(next, nextKeys, anchor!) - shifted).toBe(15);
    expect(
      restoreAnchor(next, nextKeys, anchor!) -
        next.offset(nextKeys.indexOf("k5")),
    ).toBe(3);
  });

  it("build_rows_groups_blocks_into_answer_cards_by_run_and_hides_subagent_blocks_in_the_main_lane", () => {
    const run = "00000000-0000-4000-8000-0000000000aa";
    const call = block("c", "10", { run, tool: "Agent" });
    const child = block("child", "11", { run, parent: "c", tool: "Read" });
    const text = block("t", "12", { run });
    const rows = buildRows([call, child, text], {
      now: Date.parse("2026-10-28T12:00:00.000Z"),
    });
    const answers = rows.filter((row) => row.kind === "answer");
    expect(answers).toHaveLength(1);
    const ids = answers[0]!.entries.map((entry) =>
      entry.kind === "block" ? entry.block.id : "",
    );
    expect(ids).toEqual(["c", "t"]);
    expect(ids).not.toContain("child");
  });

  it("build_rows_with_lane_root_keeps_only_descendants", () => {
    const root = block("root", "1", { tool: "Agent" });
    const child = block("child", "2", { parent: "root" });
    const grand = block("grand", "3", { parent: "child" });
    const other = block("other", "4", {});
    const rows = buildRows([root, child, grand, other], {
      laneRoot: "root",
      now: Date.parse("2026-10-28T12:00:00.000Z"),
    });
    const ids = rows.flatMap((row) =>
      row.entries.map((entry) =>
        entry.kind === "block" ? entry.block.id : "",
      ),
    );
    expect(ids).toEqual(["child", "grand"]);
  });

  it("anchor_skips_the_day_header_so_a_prepend_keeps_the_first_card", () => {
    const ix = new HeightIndex(() => 100);
    ix.setCount(3);
    ix.set(0, 32);
    const keys = ["day:Вчера", "answer:a", "answer:b"];
    const anchor = captureAnchor(ix, keys, 0);
    expect(anchor).toEqual({ key: "answer:a", offsetWithin: -32 });
    const next = new HeightIndex(() => 100);
    next.setCount(5);
    next.set(0, 32);
    const nextKeys = [
      "day:Вчера",
      "answer:p1",
      "answer:p2",
      "answer:a",
      "answer:b",
    ];
    expect(restoreAnchor(next, nextKeys, anchor!) - next.offset(3)).toBe(-32);
  });

  it("build_rows_day_keys_stay_unique_when_the_same_day_returns", () => {
    const early = block("a", "1", {});
    const mid = block("b", "2", {});
    const late = block("c", "3", {});
    if (mid.kind === "block")
      mid.block.createdAt = Date.parse("2026-10-29T12:00:00.000Z");
    if (late.kind === "block")
      late.block.createdAt = Date.parse("2026-10-28T15:00:00.000Z");
    const rows = buildRows([early, mid, late], {
      now: Date.parse("2026-10-29T18:00:00.000Z"),
    });
    const days = rows.filter((row) => row.kind === "day");
    expect(new Set(days.map((row) => row.key)).size).toBe(days.length);
    expect(days).toHaveLength(3);
    expect(days[0]?.label).toContain("Вчера");
    expect(days[2]?.label).toContain("Вчера");
    expect(days[0]?.key).not.toBe(days[2]?.key);
  });

  it("queued_message_does_not_split_the_answer_card_of_the_running_turn", () => {
    const run = "00000000-0000-4000-8000-0000000000bb";
    const entries: TimelineEntry[] = [input("m1", "1", "accepted", "feed")];
    for (let seq = 11; seq <= 40; seq++)
      entries.push(block(`b${seq}`, String(seq), { run }));
    entries.push(input("m2", "41", "queued", "strip"));
    const rows = buildRows(entries, {
      now: Date.parse("2026-10-28T12:00:00.000Z"),
    });
    const answers = rows.filter((row) => row.kind === "answer");
    expect(answers).toHaveLength(1);
    expect(answers[0]!.entries).toHaveLength(30);
    expect(rows.some((row) => row.key === "input:m2")).toBe(false);
  });

  it("tool_output_pages_256k_and_caps_the_dom_at_2mib", () => {
    const page = "a".repeat(TOOL_PAGE_BYTES);
    expect(utf8Length(page)).toBe(TOOL_PAGE_BYTES);
    let text = "";
    let capped = false;
    for (let i = 0; i < 9; i++) {
      const next = takeToolPage(text, page);
      text = next.text;
      capped = next.capped;
    }
    expect(capped).toBe(true);
    expect(utf8Length(text)).toBeLessThanOrEqual(TOOL_DOM_CAP);
    expect(utf8Length(text)).toBe(TOOL_PAGE_BYTES * 8);
  });

  it("tool_status_pill_formats_running_and_finished_durations", () => {
    const started = 0;
    expect(
      toolStatusText({
        running: true,
        error: false,
        startedAt: started,
        finishedAt: null,
        now: 4 * 60 * 1000,
      }),
    ).toBe("4 мин");
    expect(
      toolStatusText({
        running: false,
        error: false,
        startedAt: started,
        finishedAt: 200,
        now: 9_000,
      }),
    ).toBe("0,2 с");
    expect(
      toolStatusText({
        running: false,
        error: true,
        startedAt: started,
        finishedAt: 200,
        now: 9_000,
      }),
    ).toBe("Ошибка");
  });

  it("live_line_text_changes_only_on_phase_change_or_whole_seconds", () => {
    expect(liveLineText("working", 2_100)).toBe(liveLineText("working", 2_900));
    expect(liveLineText("working", 2_100)).not.toBe(
      liveLineText("working", 3_000),
    );
    expect(liveLineText("working", 2_100)).not.toBe(
      liveLineText("error", 2_100),
    );
    expect(liveLineText("working", 138_000)).toBe("Генерирует · 2 мин 18 с");
    expect(liveLineText("starting", 2_000)).toBe("Запускается · 2 с");
    expect(liveLineText("tool", 2_000, "Bash")).toBe("Выполняет Bash · 2 с");
  });

  it("activity_line_names_the_phase_before_the_first_answer", () => {
    const at = "2026-10-28T12:00:00.000Z";
    expect(
      activityLiveLine(
        { state: "working", evidence: { phase: "starting" }, since: at },
        Date.parse(at) + 2_000,
      ),
    ).toEqual({ phase: "starting", tool: "", startedAt: Date.parse(at) });
    expect(
      activityLiveLine(
        {
          state: "working",
          evidence: { phase: "running_tool", summary: "Bash", phase_since: at },
          since: at,
        },
        Date.parse(at) + 2_000,
      ),
    ).toEqual({ phase: "tool", tool: "Bash", startedAt: Date.parse(at) });
    expect(
      activityLiveLine(
        { state: "working", evidence: { phase: "generating" }, since: "" },
        9_000,
      ),
    ).toEqual({ phase: "working", tool: "", startedAt: 9_000 });
  });

  it("activity_line_never_counts_a_phase_from_before_the_current_turn", () => {
    const turn = Date.parse("2026-11-07T13:23:00.000Z");
    const stale = "2026-11-07T06:33:00.000Z";
    const line = activityLiveLine(
      {
        state: "working",
        evidence: { phase: "generating", phase_since: stale },
        since: new Date(turn).toISOString(),
      },
      turn + 3_000,
      turn,
    );
    expect(line).toEqual({ phase: "working", tool: "", startedAt: turn });
    // Без времени слота опорой остаётся начало состояния работы.
    expect(
      activityLiveLine(
        {
          state: "working",
          evidence: { phase: "generating", phase_since: stale },
          since: new Date(turn).toISOString(),
        },
        turn + 3_000,
      ).startedAt,
    ).toBe(turn);
    // Внутри хода отметка фазы уточняет начало.
    const phaseAt = turn + 60_000;
    expect(
      activityLiveLine(
        {
          state: "working",
          evidence: {
            phase: "generating",
            phase_since: new Date(phaseAt).toISOString(),
          },
          since: new Date(turn).toISOString(),
        },
        phaseAt + 2_000,
        turn,
      ).startedAt,
    ).toBe(phaseAt);
  });
});

function mulberry(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

function block(
  id: string,
  seq: string,
  opts: { run?: string; parent?: string; tool?: string },
): TimelineEntry {
  const model = new BlockModel(id, "text");
  model.inputCommandId = opts.run ?? null;
  model.parentId = opts.parent ?? null;
  model.toolName = opts.tool ?? null;
  model.createdSeq = seq;
  model.createdAt = Date.parse("2026-10-28T12:00:00.000Z");
  model.text = id;
  return { kind: "block", key: `block:${id}`, seq, block: model };
}

function input(
  key: string,
  seq: string,
  state: InputUiState,
  placement: "feed" | "strip",
): TimelineEntry {
  const view = freshInput(key, key, Date.parse("2026-10-28T12:00:00.000Z"));
  view.state = state;
  view.placement = placement;
  view.feedSeq = seq;
  return { kind: "input", key: `input:${key}`, seq, input: view };
}

it("final_tool_without_an_end_mark_shows_no_duration", () => {
  expect(toolStatusText({ running: false, error: false, startedAt: 1000, finishedAt: null, now: 90_000_000 })).toBe("");
  expect(toolStatusText({ running: true, error: false, startedAt: 0, finishedAt: null, now: 2000 })).toBe("2 с");
});
