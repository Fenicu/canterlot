import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { SvelteMap } from "svelte/reactivity";
import Timeline from "#lib/timeline/Timeline.svelte";
import { TimelineModel } from "#lib/stores/timeline.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import { RoutineReportsModel } from "#lib/routines/reports.js";
import { InterruptControl } from "#lib/composer/interrupt.svelte.js";

vi.mock("#lib/timeline/EventEntry.svelte", () => ({ default: () => undefined }));
vi.mock("#lib/timeline/UserBubble.svelte", () => ({ default: () => undefined }));

class Quiet {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

const OLD_INTERRUPT = "0198f1a0-0000-7000-8000-00000000d001";
const NOW = Date.parse("2026-11-25T10:00:00.000Z");

// Хода нет: прерванный ход и следующий за ним уже кончились.
function idleChat(interrupt: Record<string, unknown> | null): ChatStore {
  return {
    id: "c",
    timeline: new TimelineModel(),
    routineReports: new RoutineReportsModel(),
    blocks: new SvelteMap(),
    requests: { items: new SvelteMap() },
    deltaMarks: [],
    deltaSerial: 0,
    jumpRequest: null,
    takeJumpRequest: () => null,
    markRead: () => undefined,
    activity: { state: "idle", since: "2026-11-25T09:58:00.000Z" },
    queue: { queued: 0, pause_reasons: [], hold_reasons: [], run_slot: "free" },
    interrupt,
  } as unknown as ChatStore;
}

describe("interrupt outcome row", () => {
  let instance: ReturnType<typeof mount> | undefined;

  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", Quiet);
    vi.stubGlobal("IntersectionObserver", Quiet);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(4000);
  });

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.textContent = "";
  });

  function render(chat: ChatStore, interrupt: InterruptControl): HTMLElement {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(Timeline, { target, props: { chat, now: NOW, interrupt } });
    flushSync();
    return target;
  }

  const outcome = (target: HTMLElement) => target.querySelector('[data-role="interrupt-outcome"]')?.textContent?.trim() ?? null;

  it("interrupted_turn_shows_its_outcome_until_the_next_turn", () => {
    const record = { command_id: OLD_INTERRUPT, status: "completed" };
    const control = new InterruptControl({ conversationId: "c" });
    control.applyStatus(record.status, record.command_id);
    expect(outcome(render(idleChat(record), control))).toBe("Прервано");
  });

  it("outcome_of_an_old_interrupt_does_not_return_after_a_newer_turn", () => {
    const record = { command_id: OLD_INTERRUPT, status: "completed" };
    const control = new InterruptControl({ conversationId: "c" });
    control.applyStatus(record.status, record.command_id);
    // Следующий ход начался и кончился; машина и хаб досылают тот же итог прерывания.
    control.reset();
    control.applyStatus(record.status, record.command_id);
    expect(outcome(render(idleChat({ ...record }), control))).toBeNull();
  });

  it("old_interrupt_record_kept_by_the_store_does_not_mark_a_newer_turn_as_interrupted", () => {
    const record = { command_id: OLD_INTERRUPT, status: "completed" };
    const control = new InterruptControl({ conversationId: "c" });
    control.applyStatus(record.status, record.command_id);
    control.reset();
    // Хранилище держит последнюю запись прерывания до следующего снимка.
    expect(outcome(render(idleChat(record), control))).toBeNull();
  });
});
