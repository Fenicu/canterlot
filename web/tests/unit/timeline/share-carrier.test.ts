import { describe, expect, it } from "vitest";
import { BlockModel, type BlockFile } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import { cardCall } from "#lib/timeline/cards/call.js";
import { resolveCard } from "#lib/timeline/cards/registry.js";
import type { ShareData } from "#lib/timeline/cards/defs/share.js";
import { RowsCache, buildRows, isShareCarrier, type Row } from "#lib/timeline/rows.js";

// Записи как в разговоре 01a193a4…: вызов модели `mcp__canterlot__share_file`,
// блок-носитель воркера `share_file` с вложением и результат вызова.
const AT = Date.parse("2026-11-09T12:13:22.000Z");
const RUN = "01a1c039-0000-7000-8000-000000000001";

let seq = 0;

function entry(model: BlockModel): TimelineEntry {
  seq += 1;
  model.createdSeq = String(seq);
  return { kind: "block", key: `block:${model.id}`, seq: String(seq), block: model };
}

function text(id: string, run: string | null = RUN): TimelineEntry {
  const model = new BlockModel(id, "text");
  model.agent = "orchestrator";
  model.model = "claude-opus-5-5";
  model.inputCommandId = run;
  model.createdAt = AT;
  model.text = id;
  model.state = "final";
  return entry(model);
}

function mcpCall(id: string, input: { path: string; title?: string; note?: string }, opts: { run?: string | null; at?: number; tool?: string; parent?: string | null; agent?: string } = {}): TimelineEntry {
  const model = new BlockModel(id, "tool_call");
  model.toolName = opts.tool ?? "mcp__canterlot__share_file";
  model.agent = opts.agent ?? "orchestrator";
  model.model = "claude-opus-5-5";
  model.inputCommandId = opts.run === undefined ? RUN : opts.run;
  model.parentId = opts.parent ?? null;
  model.createdAt = opts.at ?? AT;
  model.text = JSON.stringify(input);
  model.state = "final";
  return entry(model);
}

function file(id: string, name: string, title = ""): BlockFile {
  return { attachment_id: id, name, mime: "text/plain", size: 195, title, note: "", width: null, height: null };
}

/** Носитель, как его пишет воркер: старый — `main` без хода, новый — ход и `orchestrator`. */
function carrier(id: string, summary: string, files: BlockFile[], opts: { run?: string | null; at?: number; agent?: string } = {}): TimelineEntry {
  const model = new BlockModel(id, "tool_call");
  model.toolName = "share_file";
  model.agent = opts.agent ?? "main";
  model.model = "";
  model.summary = summary;
  model.inputCommandId = opts.run === undefined ? null : opts.run;
  model.createdAt = opts.at ?? AT + 300;
  model.files = files;
  model.state = "final";
  return entry(model);
}

function result(id: string, call: string, attachment: string, name: string, title = ""): TimelineEntry {
  const model = new BlockModel(id, "tool_result");
  model.agent = "orchestrator";
  model.model = "claude-opus-5-5";
  model.parentId = call;
  model.inputCommandId = RUN;
  model.createdAt = AT + 400;
  model.text = JSON.stringify({ status: "shared", attachment_id: attachment, name, mime: "text/plain", size: 195, kind: "file", title, note: "" });
  model.state = "final";
  return entry(model);
}

function shown(rows: Row[]): string[] {
  return rows.flatMap((row) => row.entries.map((item) => (item.kind === "block" ? item.block.id : item.key)));
}

function answers(rows: Row[]): Row[] {
  return rows.filter((row) => row.kind === "answer");
}

function shareFiles(call: TimelineEntry, links: ReturnType<RowsCache["build"]>["links"], res: TimelineEntry | null): string[] {
  if (call.kind !== "block") throw new Error("block expected");
  const resolved = resolveCard(
    cardCall(call.block, res?.kind === "block" ? res.block : null, { root: true, carrier: links.carrier(call.block.id) }),
    AT,
  )!;
  return (resolved.data as ShareData).files.map((item) => item.attachment_id);
}

describe("share_file: one card per share", () => {
  it("history_hides_an_old_carrier_without_a_turn_behind_the_model_call", () => {
    const lead = text("lead");
    const call = mcpCall("call", { path: "/tmp/empty.txt", title: "Файл (уже не пустой)" });
    const carry = carrier("carrier", "Файл (уже не пустой)", [file("att", "empty.txt", "Файл (уже не пустой)")]);
    const res = result("res", "call", "att", "empty.txt", "Файл (уже не пустой)");
    const tail = text("tail");
    const entries = [lead, call, carry, res, tail];
    const cache = new RowsCache();
    const { rows, links } = cache.build(entries, { now: AT });

    expect(answers(rows)).toHaveLength(1);
    expect(shown(rows)).toEqual(["lead", "call", "tail"]);
    expect(links.carrier("call")?.id).toBe("carrier");
    expect(shareFiles(call, links, res)).toEqual(["att"]);
    expect(shown(buildRows(entries, { now: AT }))).toEqual(["lead", "call", "tail"]);
  });

  it("new_carrier_in_the_turn_is_hidden_too", () => {
    const call = mcpCall("call", { path: "/w/report.txt" });
    const carry = carrier("carrier", "report.txt", [file("att", "report.txt")], { run: RUN, agent: "orchestrator" });
    const { rows } = new RowsCache().build([call, carry, result("res", "call", "att", "report.txt")], { now: AT });
    expect(answers(rows)).toHaveLength(1);
    expect(shown(rows)).toEqual(["call"]);
  });

  it("live_arrival_never_shows_the_carrier_and_the_card_gets_its_file_before_the_result", () => {
    const cache = new RowsCache();
    const call = mcpCall("call", { path: "/w/report.txt" });
    const first = cache.build([call], { now: AT });
    expect(shown(first.rows)).toEqual(["call"]);

    const carry = carrier("carrier", "report.txt", [file("att", "report.txt")], { run: RUN, agent: "orchestrator" });
    const second = cache.build([call, carry], { now: AT });
    expect(shown(second.rows)).toEqual(["call"]);
    expect(shareFiles(call, second.links, null)).toEqual(["att"]);

    const res = result("res", "call", "att", "report.txt");
    const third = cache.build([call, carry, res], { now: AT });
    expect(shown(third.rows)).toEqual(["call"]);
    expect(answers(third.rows)).toHaveLength(1);
    expect(shareFiles(call, third.links, res)).toEqual(["att"]);
  });

  it("carrier_arriving_before_the_call_is_folded_once_the_call_comes", () => {
    const cache = new RowsCache();
    const lead = text("lead");
    const carry = carrier("carrier", "report.txt", [file("att", "report.txt")], { run: RUN, agent: "orchestrator", at: AT });
    const first = cache.build([lead, carry], { now: AT });
    expect(shown(first.rows)).toEqual(["lead", "carrier"]);

    const call = mcpCall("call", { path: "/w/report.txt" }, { at: AT + 10 });
    const second = cache.build([lead, carry, call], { now: AT });
    expect(shown(second.rows)).toEqual(["lead", "call"]);
    expect(second.links.carrier("call")?.id).toBe("carrier");
    expect(shown(second.rows)).toEqual(shown(buildRows([lead, carry, call], { now: AT })));
  });

  it("an_old_carrier_without_a_turn_is_not_taken_by_a_later_call", () => {
    const cache = new RowsCache();
    const carry = carrier("carrier", "report.txt", [file("att", "report.txt")], { at: AT });
    cache.build([carry], { now: AT });
    const call = mcpCall("call", { path: "/w/report.txt" }, { at: AT + 10 });
    const { rows, links } = cache.build([carry, call], { now: AT });
    expect(shown(rows)).toEqual(["carrier", "call"]);
    expect(links.carrier("call")).toBeNull();
  });

  it("parallel_shares_pair_by_title_even_when_carriers_come_in_another_order", () => {
    const a = mcpCall("call-a", { path: "/w/a.txt", title: "Первый" });
    const b = mcpCall("call-b", { path: "/w/b.txt", title: "Второй" });
    const cb = carrier("carrier-b", "Второй", [file("att-b", "b.txt", "Второй")]);
    const ca = carrier("carrier-a", "Первый", [file("att-a", "a.txt", "Первый")]);
    const { rows, links } = new RowsCache().build([a, b, cb, ca], { now: AT });
    expect(shown(rows)).toEqual(["call-a", "call-b"]);
    expect(links.carrier("call-a")?.id).toBe("carrier-a");
    expect(links.carrier("call-b")?.id).toBe("carrier-b");
    expect(shareFiles(a, links, null)).toEqual(["att-a"]);
  });

  it("the_answer_wins_over_a_mismatched_carrier", () => {
    const a = mcpCall("call-a", { path: "/w/same.txt" });
    const carry = carrier("carrier", "same.txt", [file("att-other", "same.txt")]);
    const res = result("res", "call-a", "att-own", "same.txt");
    const { links } = new RowsCache().build([a, carry, res], { now: AT });
    expect(shareFiles(a, links, res)).toEqual(["att-own"]);
  });

  it("a_carrier_without_a_call_stays_a_card_of_its_own", () => {
    const carry = carrier("carrier", "Отчёт", [file("att", "report.txt", "Отчёт")]);
    const { rows } = new RowsCache().build([text("lead", null), carry], { now: AT });
    expect(shown(rows)).toContain("carrier");
  });

  it("does_not_pair_across_turns_or_far_in_time", () => {
    const call = mcpCall("call", { path: "/w/report.txt" });
    const other = carrier("other-turn", "report.txt", [file("att", "report.txt")], { run: "another-run", agent: "orchestrator" });
    const late = carrier("late", "report.txt", [file("att2", "report.txt")], { at: AT + 2 * 3_600_000 });
    const { rows } = new RowsCache().build([call, other, late], { now: AT });
    expect(shown(rows)).toEqual(["call", "other-turn", "late"]);
  });

  it("a_subagent_share_folds_its_carrier_and_keeps_the_lane_free_of_it", () => {
    const task = mcpCall("task", { path: "" }, { tool: "Agent" });
    const call = mcpCall("call", { path: "/w/report.txt" }, { parent: "task", agent: "Explore" });
    const carry = carrier("carrier", "report.txt", [file("att", "report.txt")], { run: RUN, agent: "orchestrator" });
    const entries = [task, call, carry];
    const { rows, links } = new RowsCache().build(entries, { now: AT });
    expect(shown(rows)).toEqual(["task"]);
    expect(links.children("task").map((child) => child.id)).toEqual(["call"]);
    expect(shown(buildRows(entries, { now: AT, laneRoot: "task" }))).toEqual(["call"]);
  });

  it("codex_and_opencode_names_pair_as_well", () => {
    for (const tool of ["canterlot.share_file", "canterlot_share_file"]) {
      const call = mcpCall(`call-${tool}`, { path: "/w/report.txt" }, { tool });
      const carry = carrier(`carrier-${tool}`, "report.txt", [file("att", "report.txt")]);
      const { rows } = new RowsCache().build([call, carry], { now: AT });
      expect(shown(rows)).toEqual([`call-${tool}`]);
    }
  });

  it("a_window_trim_that_drops_the_call_brings_the_carrier_back", () => {
    const cache = new RowsCache();
    const call = mcpCall("call", { path: "/w/report.txt" });
    const carry = carrier("carrier", "report.txt", [file("att", "report.txt")]);
    const tail = text("tail");
    cache.build([call, carry, tail], { now: AT });
    const trimmed = cache.build([carry, tail], { now: AT });
    expect(shown(trimmed.rows)).toEqual(shown(buildRows([carry, tail], { now: AT })));
    expect(shown(trimmed.rows)).toContain("carrier");
  });

  it("the_model_call_is_never_taken_for_a_carrier", () => {
    const call = mcpCall("call", { path: "/w/report.txt" });
    expect(call.kind === "block" && isShareCarrier(call.block)).toBe(false);
    const carry = carrier("carrier", "report.txt", []);
    expect(carry.kind === "block" && isShareCarrier(carry.block)).toBe(true);
  });
});

// Codex зовёт MCP `сервер.инструмент` (codex/events.rs, `mcp_summary`): вызов модели —
// `canterlot.share_file` от `orchestrator`; носитель воркера 1.24.1 — `share_file`
// без модели, без хода и с меткой `main`.
function codexText(id: string, run: string | null = RUN): TimelineEntry {
  const made = text(id, run);
  if (made.kind === "block") made.block.model = "gpt-5.5-codex";
  return made;
}

function codexCall(id: string, input: { path: string; title?: string }, opts: { run?: string | null; at?: number } = {}): TimelineEntry {
  const made = mcpCall(id, input, { ...opts, tool: "canterlot.share_file" });
  if (made.kind === "block") made.block.model = "gpt-5.5-codex";
  return made;
}

function runOf(row: Row): string | null {
  for (const item of row.entries) if (item.kind === "block" && item.block.inputCommandId) return item.block.inputCommandId;
  return null;
}

describe("share_file: an unpaired carrier has no group of its own", () => {
  const TITLE = "Улучшенный скриншот — JPG";
  const JPG = { attachment_id: "att", name: "shot.jpg", mime: "image/jpeg", size: 1_363_148, title: TITLE, note: "", width: 1920, height: 1080 };

  it("codex_carrier_far_from_its_call_joins_the_answer_before_it", () => {
    // Пара не сложилась (больше получаса после вызова): носитель встаёт в карточку хода рядом.
    const call = codexCall("call", { path: "/tmp/shot.jpg", title: TITLE });
    const said = codexText("said");
    const carry = carrier("carrier", TITLE, [JPG], { at: AT + 45 * 60_000 });
    const entries = [call, said, carry];
    for (const rows of [new RowsCache().build(entries, { now: AT }).rows, buildRows(entries, { now: AT })]) {
      expect(answers(rows)).toHaveLength(1);
      expect(shown(rows)).toEqual(["call", "said", "carrier"]);
      expect(runOf(answers(rows)[0]!)).toBe(RUN);
    }
  });

  it("codex_carrier_whose_call_is_on_an_older_page_joins_the_loaded_answer", () => {
    const said = codexText("said");
    const carry = carrier("carrier", TITLE, [JPG]);
    const { rows } = new RowsCache().build([said, carry], { now: AT });
    expect(answers(rows)).toHaveLength(1);
    expect(runOf(answers(rows)[0]!)).toBe(RUN);
  });

  it("live_carrier_after_an_answer_joins_it_without_a_rebuild", () => {
    const cache = new RowsCache();
    const said = codexText("said");
    cache.build([said], { now: AT });
    const carry = carrier("carrier", TITLE, [JPG]);
    const { rows } = cache.build([said, carry], { now: AT });
    expect(rows).toEqual(buildRows([said, carry], { now: AT }));
    expect(answers(rows)).toHaveLength(1);
  });

  it("the_owner_message_between_keeps_the_carrier_apart_and_it_has_no_run", () => {
    const said = codexText("said");
    const owner: TimelineEntry = {
      kind: "input",
      key: "input:owner",
      seq: "99",
      input: { placement: "feed", origin: { kind: "owner" }, createdAt: AT + 100 } as never,
    };
    const carry = carrier("carrier", TITLE, [JPG]);
    const rows = answers(buildRows([said, owner, carry], { now: AT }));
    expect(rows).toHaveLength(2);
    expect(runOf(rows[1]!)).toBeNull();
  });

  it("a_subagent_answer_does_not_take_the_carrier", () => {
    const sub = codexText("sub");
    if (sub.kind === "block") sub.block.agent = "Explore";
    const carry = carrier("carrier", TITLE, [JPG]);
    expect(answers(buildRows([sub, carry], { now: AT }))).toHaveLength(2);
  });

  it("an_ordinary_block_without_a_turn_still_starts_its_own_card", () => {
    const said = codexText("said");
    const loose = codexText("loose", null);
    expect(answers(buildRows([said, loose], { now: AT }))).toHaveLength(2);
  });
});

