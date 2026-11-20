import { expect, test } from "vitest";
import {
  PRESENCE_HEARTBEAT_MS,
  PRESENCE_IDLE_MS,
  trackChatPresence,
} from "../../src/lib/presence";

const MIN = 60_000;

function setup() {
  let now = 1_000_000;
  let state: "visible" | "hidden" = "visible";
  const doc = new EventTarget() as EventTarget & { visibilityState: string };
  Object.defineProperty(doc, "visibilityState", { get: () => state });
  const win = new EventTarget() as EventTarget & {
    setInterval: (fn: () => void, ms: number) => number;
    clearInterval: (id: number) => void;
  };
  let tick: (() => void) | null = null;
  win.setInterval = (fn) => {
    tick = fn;
    return 1;
  };
  win.clearInterval = () => {
    tick = null;
  };
  const sent: boolean[] = [];
  const stop = trackChatPresence("c1", {
    send: (_id, visible) => sent.push(visible),
    doc: doc as unknown as Document,
    win: win as unknown as Window,
    now: () => now,
    intervalMs: PRESENCE_HEARTBEAT_MS,
  });
  const advance = (ms: number) => {
    let left = ms;
    while (left > 0) {
      const step = Math.min(left, PRESENCE_HEARTBEAT_MS);
      now += step;
      left -= step;
      if (step === PRESENCE_HEARTBEAT_MS) tick?.();
    }
  };
  return {
    sent,
    stop,
    advance,
    doc,
    win,
    setState: (next: "visible" | "hidden") => {
      state = next;
      doc.dispatchEvent(new Event("visibilitychange"));
    },
  };
}

test("an_untouched_visible_chat_reports_not_visible_once_after_15_minutes", () => {
  const t = setup();
  t.advance(PRESENCE_IDLE_MS - PRESENCE_HEARTBEAT_MS);
  expect(t.sent.every((v) => v)).toBe(true);
  const trues = t.sent.length;
  t.advance(PRESENCE_HEARTBEAT_MS);
  expect(t.sent.length).toBe(trues + 1);
  expect(t.sent.at(-1)).toBe(false);
  t.advance(10 * MIN);
  expect(t.sent.length).toBe(trues + 1);
  t.stop();
});

test("activity_restarts_the_idle_window", () => {
  const t = setup();
  t.advance(14 * MIN);
  t.doc.dispatchEvent(new Event("keydown"));
  t.advance(6 * MIN);
  expect(t.sent.every((v) => v)).toBe(true);
  t.advance(9 * MIN);
  expect(t.sent.filter((v) => !v)).toHaveLength(1);
  expect(t.sent.at(-1)).toBe(false);
  t.stop();
});

test("the_first_activity_after_idle_reports_visible_at_once_and_resumes_heartbeats", () => {
  const t = setup();
  t.advance(PRESENCE_IDLE_MS + PRESENCE_HEARTBEAT_MS);
  expect(t.sent.at(-1)).toBe(false);
  const before = t.sent.length;
  t.doc.dispatchEvent(new Event("pointerdown"));
  expect(t.sent.length).toBe(before + 1);
  expect(t.sent.at(-1)).toBe(true);
  t.doc.dispatchEvent(new Event("pointerdown"));
  expect(t.sent.length).toBe(before + 1);
  t.advance(PRESENCE_HEARTBEAT_MS);
  expect(t.sent.length).toBe(before + 2);
  expect(t.sent.at(-1)).toBe(true);
  t.stop();
});

test("returning_to_the_tab_after_idle_counts_as_activity", () => {
  const t = setup();
  t.advance(PRESENCE_IDLE_MS + PRESENCE_HEARTBEAT_MS);
  expect(t.sent.at(-1)).toBe(false);
  t.setState("hidden");
  const before = t.sent.length;
  t.setState("visible");
  expect(t.sent.length).toBe(before + 1);
  expect(t.sent.at(-1)).toBe(true);
  t.stop();
});

test("cleanup_removes_the_activity_listeners", () => {
  const t = setup();
  t.stop();
  const before = t.sent.length;
  for (const type of ["pointerdown", "pointermove", "keydown", "wheel", "touchstart", "scroll"]) {
    t.doc.dispatchEvent(new Event(type));
  }
  expect(t.sent.length).toBe(before);
});
