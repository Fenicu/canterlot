import { describe, expect, it } from "vitest";
import {
  RoutineReportsModel,
  failureText,
  placeReports,
  type RoutineReport
} from "#lib/routines/reports.js";
import type { Row } from "#lib/timeline/rows.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";

const CONVERSATION = "00000000-0000-4000-8000-0000000000a1";

function report(id: string, patch: Partial<RoutineReport> = {}): RoutineReport {
  return {
    id,
    conversation_id: CONVERSATION,
    schedule_id: "00000000-0000-4000-8000-0000000000b1",
    run_id: null,
    title: "Утренний стендап",
    state: "pending",
    body: null,
    error: null,
    created_at: "2026-11-18T09:00:00.000Z",
    completed_at: null,
    ...patch
  };
}

describe("RoutineReportsModel", () => {
  it("keeps cards from the snapshot ordered by creation time", () => {
    const model = new RoutineReportsModel();
    model.ingestSnapshot([
      report("b", { created_at: "2026-11-18T10:00:00.000Z" }),
      report("a", { created_at: "2026-11-18T09:00:00.000Z" })
    ]);
    expect(model.list.map((item) => item.id)).toEqual(["a", "b"]);
  });

  it("moves a card from pending to ready on the changed event", () => {
    const model = new RoutineReportsModel();
    model.apply(report("a"));
    model.apply(report("a", { state: "ready", body: "## Итоги", completed_at: "2026-11-18T09:05:00.000Z" }));
    expect(model.list).toHaveLength(1);
    expect(model.list[0]).toMatchObject({ state: "ready", body: "## Итоги" });
  });

  it("drops a card on dismiss and does not resurrect it from a late changed event", () => {
    const model = new RoutineReportsModel();
    model.apply(report("a"));
    model.dismiss("a");
    expect(model.list).toHaveLength(0);
    model.apply(report("a", { state: "ready", body: "поздно" }));
    expect(model.list).toHaveLength(0);
  });

  it("replaces the whole set from a fresh snapshot but keeps dismissals made after it", () => {
    const model = new RoutineReportsModel();
    model.apply(report("a"));
    model.apply(report("b", { created_at: "2026-11-18T10:00:00.000Z" }));
    model.dismiss("a");
    model.ingestSnapshot([report("a"), report("b", { created_at: "2026-11-18T10:00:00.000Z" })]);
    expect(model.list.map((item) => item.id)).toEqual(["b"]);
    model.ingestSnapshot([]);
    expect(model.list).toHaveLength(0);
  });

  it("clears everything when the conversation changes", () => {
    const model = new RoutineReportsModel();
    model.apply(report("a"));
    model.clear();
    expect(model.list).toHaveLength(0);
    model.apply(report("a"));
    expect(model.list).toHaveLength(1);
  });
});

describe("failureText", () => {
  it("speaks plainly for every known reason", () => {
    expect(failureText({ reason: "turn_timeout", message: "x" })).toBe("Копия сессии не ответила за 60 минут");
    expect(failureText({ reason: "parent_busy_timeout", message: "x" })).toBe("Сессия была занята больше двух часов, отчёт не начался");
    expect(failureText({ reason: "copy_failed", message: "x" })).toBe("Копия сессии завершилась с ошибкой");
    expect(failureText({ reason: "copy_lost", message: "x" })).toBe("Копия сессии пропала до конца отчёта");
    expect(failureText({ reason: "empty_report", message: "x" })).toBe("Копия закончила работу без текста ответа");
    expect(failureText({ reason: "no_native_session", message: "x" })).toBe("У сессии ещё нет записи, которую можно прочитать");
  });

  it("falls back to the hub line for an unknown reason and to a generic one without error", () => {
    expect(failureText({ reason: "something_new", message: "Строка от хаба" })).toBe("Строка от хаба");
    expect(failureText({ reason: "something_new", message: "" })).toBe("Отчёт не собран");
    expect(failureText(null)).toBe("Отчёт не собран");
  });
});

function inputRow(key: string, createdAt: number): Row {
  const entry = { kind: "input", key, seq: "1", input: { createdAt } } as unknown as TimelineEntry;
  return { key, kind: "input", entries: [entry] };
}
const day = (key: string): Row => ({ key, kind: "day", entries: [], label: key });
const at = (iso: string): number => Date.parse(iso);

describe("placeReports", () => {
  const rows = [
    day("day:1"),
    inputRow("m1", at("2026-11-18T08:00:00.000Z")),
    inputRow("m2", at("2026-11-18T10:00:00.000Z"))
  ];

  it("puts a card between the rows around its creation time", () => {
    const placed = placeReports(rows, [report("a")], { hasOlder: false, tailDetached: false });
    expect(placed.map((row) => row.key)).toEqual(["day:1", "m1", "routine:a", "m2"]);
    expect(placed[2]).toMatchObject({ kind: "report" });
  });

  it("puts a newer card at the end of the feed", () => {
    const placed = placeReports(rows, [report("a", { created_at: "2026-11-18T12:00:00.000Z" })], { hasOlder: false, tailDetached: false });
    expect(placed.at(-1)?.key).toBe("routine:a");
  });

  it("does not split a day label from its first row", () => {
    const placed = placeReports(rows, [report("a", { created_at: "2026-11-18T07:00:00.000Z" })], { hasOlder: false, tailDetached: false });
    expect(placed.map((row) => row.key)).toEqual(["routine:a", "day:1", "m1", "m2"]);
  });

  it("hides a card newer than the window when the window is detached from the tail", () => {
    const placed = placeReports(rows, [report("a", { created_at: "2026-11-18T12:00:00.000Z" })], { hasOlder: false, tailDetached: true });
    expect(placed.map((row) => row.key)).toEqual(["day:1", "m1", "m2"]);
  });

  it("returns the very same rows when there are no cards", () => {
    expect(placeReports(rows, [], { hasOlder: false, tailDetached: false })).toBe(rows);
  });

  it("orders several cards by time", () => {
    const placed = placeReports(
      rows,
      [report("b", { created_at: "2026-11-18T11:00:00.000Z" }), report("a", { created_at: "2026-11-18T09:00:00.000Z" })],
      { hasOlder: false, tailDetached: false }
    );
    expect(placed.map((row) => row.key)).toEqual(["day:1", "m1", "routine:a", "m2", "routine:b"]);
  });
});
