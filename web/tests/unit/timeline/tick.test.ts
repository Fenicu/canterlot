import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { SvelteMap } from "svelte/reactivity";
import Timeline from "#lib/timeline/Timeline.svelte";
import { TimelineModel } from "#lib/stores/timeline.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import { ticking } from "./ticking.svelte";

const rows = vi.hoisted(() => ({ calls: 0 }));

vi.mock("#lib/timeline/rows.js", async (original) => {
  const actual = await original<typeof import("#lib/timeline/rows.js")>();
  return {
    ...actual,
    buildRows: (...args: Parameters<typeof actual.buildRows>) => {
      rows.calls += 1;
      return actual.buildRows(...args);
    },
  };
});

vi.mock("#lib/timeline/AnswerCard.svelte", () => ({ default: () => undefined }));
vi.mock("#lib/timeline/UserBubble.svelte", () => ({ default: () => undefined }));

class Quiet {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", Quiet);
  vi.stubGlobal("IntersectionObserver", Quiet);
});

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.textContent = "";
});

test("clock_ticks_do_not_rebuild_rows_until_the_day_changes", () => {
  const chat = {
    id: "c",
    timeline: new TimelineModel(),
    blocks: new SvelteMap(),
    deltaMarks: [],
    deltaSerial: 0,
  } as unknown as ChatStore;
  const noon = new Date(2026, 9, 28, 12, 0, 0).getTime();
  const now = ticking(noon);
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(Timeline, {
    target,
    props: {
      chat,
      get now() {
        return now.value;
      },
    },
  });
  flushSync();
  const before = rows.calls;
  expect(before).toBeGreaterThan(0);
  for (let second = 1; second <= 5; second++) {
    now.value = noon + second * 1000;
    flushSync();
  }
  expect(rows.calls).toBe(before);
  now.value = noon + 24 * 3_600_000;
  flushSync();
  expect(rows.calls).toBe(before + 1);
  unmount(instance);
});
