import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import type { InputView } from "#lib/stores/inputs.js";
import { createRequestItem, type RequestClientState, type RequestItem } from "#lib/stores/requests.svelte.js";
import type { CompactionModel, TimelineEntry } from "#lib/stores/timeline.js";
import { RowsCache, buildRows, type Row } from "#lib/timeline/rows.js";

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

function request(id: string, seq: string, state: RequestClientState = "open"): TimelineEntry {
  const item: RequestItem = createRequestItem({
    id,
    conversationId: "c",
    kind: "question",
    state,
    payload: {},
    winnerDecision: state === "saved" ? { answers: {} } : undefined,
  });
  return { kind: "request", key: `request:${id}`, seq, request: item };
}

function input(key: string, seq: string, placement: InputView["placement"] = "feed", state: InputView["state"] = "accepted"): TimelineEntry {
  const view: InputView = {
    key,
    id: key,
    text: key,
    attachments: [],
    state,
    revision: 1,
    placement,
    marks: { saved: true, accepted: state === "accepted" },
    cancelRequested: false,
    attempt: null,
    createdSeq: seq,
    feedSeq: placement === "feed" ? seq : null,
    createdAt: NOW,
    retryOf: null,
    origin: null,
    localOnly: false,
    optimistic: false,
    stripUntil: null,
  };
  return { kind: "input", key: `input:${key}`, seq, input: view };
}

function compaction(id: string, seq: string): TimelineEntry {
  const model: CompactionModel = {
    id,
    trigger: "auto",
    preTokens: null,
    ok: true,
    createdAt: NOW,
    finishedAt: NOW,
  };
  return { kind: "compaction", key: `compaction:${id}`, seq, compaction: model };
}

function ids(rows: Row[]): string[] {
  return rows.flatMap((row) =>
    row.entries.map((entry) => (entry.kind === "block" ? entry.block.id : entry.key)),
  );
}

/** Слепок без ссылок на прокси: ключи строк и состояние записей, которые видит лента. */
function view(rows: Row[]): unknown {
  return rows.map((row) => ({
    key: row.key,
    kind: row.kind,
    label: row.label ?? null,
    entries: row.entries.map((entry) => {
      if (entry.kind === "block") return `block:${entry.block.id}:${entry.block.state}`;
      if (entry.kind === "request") return `request:${entry.request.id}:${entry.request.state}`;
      if (entry.kind === "input") return `input:${entry.input.key}:${entry.input.placement}:${entry.input.state}`;
      return `compaction:${entry.compaction.id}`;
    }),
  }));
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
      expect(view(cached.rows)).toEqual(view(full));
    }
    for (let index = 40; index < 60; index++) {
      entries.push(block(`b${index}`, String(index + 1), null));
      if (entries.length > 30) entries.shift();
      const cached = cache.build([...entries], { now: NOW });
      const full = buildRows(entries, { now: NOW });
      expect(view(cached.rows)).toEqual(view(full));
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

  it("moves_an_answered_request_from_the_dock_into_its_card", () => {
    const cache = new RowsCache();
    const call = block("call", "1", "run-1");
    const ask = request("r1", "2", "open");
    const entries = [call, ask];
    const first = cache.build(entries, { now: NOW });
    expect(ids(first.rows)).toEqual(["call"]);

    if (ask.kind === "request") {
      ask.request.state = "saved";
      ask.request.winnerDecision = { answers: { q1: "да" } };
    }

    const next = cache.build(entries, { now: NOW });
    const full = buildRows(entries, { now: NOW });
    expect(view(next.rows)).toEqual(view(full));
    expect(ids(next.rows)).toEqual(["call", "request:r1"]);
    expect(next.rows.at(-1)?.entries.map((entry) => entry.key)).toEqual([
      "block:call",
      "request:r1",
    ]);
  });

  it("keeps_a_request_row_after_its_state_and_following_entries_change", () => {
    const cache = new RowsCache();
    const call = block("call", "1", "run-1");
    const ask = request("r1", "2", "saved");
    const entries = [call, ask];
    cache.build(entries, { now: NOW });

    if (ask.kind === "request") ask.request.state = "applied";
    entries.push(block("next", "3", "run-1"));
    const next = cache.build(entries, { now: NOW });
    const full = buildRows(entries, { now: NOW });
    expect(view(next.rows)).toEqual(view(full));
    expect(next.rows.at(-1)?.entries.map((entry) => entry.key)).toEqual([
      "block:call",
      "request:r1",
      "block:next",
    ]);
  });

  for (const initial of [0x5eed, 0xbeef, 0x1234]) {
    it(`matches_a_full_rebuild_through_random_appends_updates_and_trims_${initial.toString(16)}`, () => {
      let seed = initial;
      const random = (): number => {
        seed = (seed * 1664525 + 1013904223) >>> 0;
        return seed / 0x1_0000_0000;
      };
      const pick = <T>(list: T[]): T => list[Math.floor(random() * list.length)]!;
      const cache = new RowsCache();
      let entries: TimelineEntry[] = [];
      let sequence = 0;
      let counter = 0;
      let run = "run-0";
      const seq = (): string => String(++sequence);

      for (let step = 0; step < 240; step++) {
        const action = random();
        if (action < 0.3 || entries.length === 0) {
          const roll = random();
          if (roll < 0.45) {
            // Продолжение карточки: блок того же хода.
            entries = [...entries, block(`b${++counter}`, seq(), run)];
          } else if (roll < 0.6) {
            // Новая карточка хода.
            run = `run-${++counter}`;
            entries = [...entries, block(`b${++counter}`, seq(), run)];
          } else if (roll < 0.72) {
            // Шаг субагента: в строки не попадает, но меняет связи карточки.
            const parent = entries.findLast((entry) => entry.kind === "block" && entry.block.parentId === null);
            entries = [
              ...entries,
              block(
                `b${++counter}`,
                seq(),
                run,
                parent?.kind === "block" ? parent.block.id : null,
              ),
            ];
          } else if (roll < 0.84) {
            entries = [...entries, request(`r${++counter}`, seq(), random() < 0.5 ? "open" : "saved")];
          } else if (roll < 0.92) {
            entries = [
              ...entries,
              input(
                `i${++counter}`,
                seq(),
                random() < 0.7 ? "feed" : "strip",
                pick<InputView["state"]>(["queued", "accepted", "transmitting"]),
              ),
            ];
          } else {
            entries = [...entries, compaction(`c${++counter}`, seq())];
          }
        } else if (action < 0.65) {
          // Обновление уже лежащей записи на месте.
          const at = Math.floor(random() * entries.length);
          const entry = entries[at]!;
          if (entry.kind === "request") {
            entry.request.state = pick<RequestClientState>(["saved", "transmitting", "applied", "closed"]);
          } else if (entry.kind === "input") {
            entry.input.placement = pick<InputView["placement"]>(["feed", "strip", "none"]);
            entry.input.state = pick<InputView["state"]>(["queued", "transmitting", "accepted", "rejected"]);
          } else if (entry.kind === "block") {
            entry.block.state = pick(["streaming", "final", "incomplete"]);
            if (random() < 0.25) entry.block.toolName = pick(["Read", "Agent", "Bash"]);
            if (random() < 0.2) {
              const parent = entries.findLast((item) => item.kind === "block" && item !== entry && item.block.parentId === null);
              entry.block.parentId = random() < 0.5 && parent?.kind === "block" ? parent.block.id : null;
            }
          }
        } else if (action < 0.8) {
          // Запись выпала из окна слева.
          entries = entries.slice(1);
        } else if (action < 0.9) {
          // Старшая страница пришла в начало.
          const older = block(`old${++counter}`, String(-counter), null);
          if (older.kind === "block") older.block.createdAt = NOW - 24 * 3_600_000;
          entries = [older, ...entries];
        } else {
          // Ревизия уже лежащей записи приходит новой записью с тем же ключом.
          const at = Math.floor(random() * entries.length);
          const entry = entries[at]!;
          entries = entries.map((item) =>
            item === entry ? { ...entry, seq: seq() } : item,
          );
        }

        const cached = cache.build(entries, { now: NOW });
        const full = buildRows(entries, { now: NOW });
        expect(view(cached.rows), `шаг ${step}`).toEqual(view(full));
      }
    });
  }
});
