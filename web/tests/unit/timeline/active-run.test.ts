import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { SvelteMap } from "svelte/reactivity";
import Timeline from "#lib/timeline/Timeline.svelte";
import { BlockModel } from "#lib/stores/blocks.js";
import { TimelineModel, type TimelineEntry } from "#lib/stores/timeline.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import { RoutineReportsModel } from "#lib/routines/reports.js";
import { liveSlot } from "./live-run.svelte";
import { liveLineText } from "#lib/timeline/present.js";

vi.mock("#lib/timeline/EventEntry.svelte", () => ({ default: () => undefined }));
vi.mock("#lib/timeline/UserBubble.svelte", () => ({ default: () => undefined }));

class Quiet {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

// Ввод и его команда — разные id: слот очереди называет ввод, блоки хода несут команду.
const INPUT = "0198f1a0-0000-7000-8000-00000000a001";
const COMMAND = "0198f1a0-0000-7000-8000-00000000c001";
const NOW = Date.parse("2026-11-25T10:00:00.000Z");

let seq = 0;
function block(kind: BlockModel["kind"], opts: { tool?: string; text?: string; state?: "streaming" | "final"; parent?: string } = {}): TimelineEntry {
  seq += 1;
  const model = new BlockModel(`b${seq}`, kind);
  model.toolName = opts.tool ?? null;
  model.parentId = opts.parent ?? null;
  model.summary = opts.tool ? "cargo test" : null;
  model.text = opts.text ?? "";
  model.state = opts.state ?? "final";
  model.inputCommandId = COMMAND;
  model.createdSeq = String(seq);
  model.createdAt = NOW - 60_000 + seq * 1000;
  model.startedAt = model.createdAt;
  model.finishedAt = opts.state === "streaming" ? null : model.createdAt;
  return { kind: "block", key: `block:${model.id}`, seq: String(seq), block: model };
}

function chatOf(slot: ReturnType<typeof liveSlot>, timeline: TimelineModel): ChatStore {
  return {
    id: "c",
    timeline,
    routineReports: new RoutineReportsModel(),
    blocks: new SvelteMap(),
    requests: { items: new SvelteMap() },
    deltaMarks: [],
    deltaSerial: 0,
    jumpRequest: null,
    takeJumpRequest: () => null,
    markRead: () => undefined,
    get activity() {
      return slot.activity;
    },
    get queue() {
      return slot.queue;
    },
  } as unknown as ChatStore;
}

describe("live turn whose run slot names the input, not its command", () => {
  let instance: ReturnType<typeof mount> | undefined;

  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", Quiet);
    vi.stubGlobal("IntersectionObserver", Quiet);
    // Высокий экран: лента рисует все строки, а не одну последнюю.
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(4000);
    seq = 0;
  });

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.textContent = "";
  });

  function render(entries: TimelineEntry[], slot = liveSlot({ input: INPUT, command: COMMAND })) {
    const timeline = new TimelineModel();
    timeline.applyHistoryPage(entries, "tail");
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(Timeline, { target, props: { chat: chatOf(slot, timeline), now: NOW } });
    flushSync();
    return { target, timeline, slot };
  }

  const card = (target: HTMLElement) => target.querySelector<HTMLElement>('[data-kind="answer"]');
  const detached = (target: HTMLElement) => target.querySelector('[data-role="live-turn"]');
  const liveMode = (target: HTMLElement) => {
    const answer = card(target);
    return {
      window: answer?.querySelector('[data-role="live-window-head"]') !== null,
      line: answer?.querySelector("[data-working]") !== null,
    };
  };

  it("a_running_tool_without_a_streaming_block_keeps_the_card_live_and_no_line_detached", () => {
    const { target } = render([block("text", { text: "Проверю тесты." }), block("tool_call", { tool: "Bash" })]);

    expect(liveMode(target)).toEqual({ window: true, line: true });
    expect(detached(target)).toBeNull();
    expect(target.querySelectorAll("[data-working]")).toHaveLength(1);
  });

  it("streaming_then_a_bare_tool_call_then_streaming_again_never_leaves_the_live_window", () => {
    const first = block("reasoning", { text: "Смотрю код", state: "streaming" });
    const { target, timeline } = render([first]);
    const modes = [liveMode(target)];
    const lines = [detached(target)];

    if (first.kind === "block") first.block.state = "final";
    timeline.upsert(block("tool_call", { tool: "Bash" }));
    flushSync();
    modes.push(liveMode(target));
    lines.push(detached(target));

    timeline.upsert(block("text", { text: "Тесты", state: "streaming" }));
    flushSync();
    modes.push(liveMode(target));
    lines.push(detached(target));

    for (const mode of modes) expect(mode).toEqual({ window: true, line: true });
    expect(lines).toEqual([null, null, null]);
  });

  it("the_earlier_card_of_a_turn_split_by_compaction_settles_while_its_tool_runs", () => {
    // Сжатие контекста режет ход на две карточки; в первой завис блок в `streaming`.
    const earlier = block("reasoning", { text: "До сжатия", state: "streaming" });
    seq += 1;
    const compaction: TimelineEntry = {
      kind: "compaction",
      key: "compaction:k",
      seq: String(seq),
      compaction: { id: "k", trigger: "auto", preTokens: null, ok: true, createdAt: NOW - 30_000, finishedAt: NOW - 29_000 },
    };
    const { target } = render([earlier, compaction, block("text", { text: "После сжатия" }), block("tool_call", { tool: "Bash" })]);

    const cards = [...target.querySelectorAll<HTMLElement>('[data-kind="answer"]')];
    expect(cards).toHaveLength(2);
    expect(cards[0]!.querySelector("[data-working]")).toBeNull();
    expect(cards[1]!.querySelector('[data-role="live-window-head"]')).not.toBeNull();
    expect(cards[1]!.querySelector("[data-working]")).not.toBeNull();
    expect(detached(target)).toBeNull();
  });

  it("a_turn_the_cli_started_without_a_slot_still_finds_its_card_by_time", () => {
    const slot = liveSlot({ input: null, command: null, since: "2026-11-25T09:58:00.000Z" });
    const { target } = render([block("text", { text: "Сам начал." }), block("tool_call", { tool: "Bash" })], slot);

    expect(liveMode(target)).toEqual({ window: true, line: true });
    expect(detached(target)).toBeNull();
  });

  it("a_stalled_turn_in_the_slot_keeps_the_line_in_the_card", () => {
    // `turnIsLive` считает `stalled` идущим ходом: лента не рисует свою строку, значит, её держит карточка.
    const slot = liveSlot({ input: INPUT, command: COMMAND, state: "stalled", phase: "generating" });
    const { target } = render([block("text", { text: "Думаю." }), block("tool_call", { tool: "Bash" })], slot);

    expect(liveMode(target)).toEqual({ window: true, line: true });
    expect(detached(target)).toBeNull();
  });

  it("a_cli_turn_keeps_its_card_when_since_moves_past_the_last_block", () => {
    const slot = liveSlot({ input: null, command: null, since: "2026-11-25T09:58:00.000Z", phase: "generating" });
    const { target, timeline } = render([block("text", { text: "Сам начал." }), block("tool_call", { tool: "Bash" })], slot);
    expect(liveMode(target)).toEqual({ window: true, line: true });

    // Смена фазы без нового блока: `since` уходит за последний блок хода.
    slot.shift({ since: "2026-11-25T09:59:59.000Z", phase: "running_tool" });
    flushSync();
    expect(liveMode(target)).toEqual({ window: true, line: true });
    expect(detached(target)).toBeNull();

    // То же через другое состояние: ожидание владельца и возврат к работе.
    slot.shift({ state: "waiting_for_user", since: "2026-11-25T10:00:00.000Z", phase: "permission" });
    flushSync();
    slot.shift({ since: "2026-11-25T10:00:01.000Z", phase: "generating" });
    flushSync();
    expect(liveMode(target)).toEqual({ window: true, line: true });
    expect(detached(target)).toBeNull();

    const done = block("text", { text: "Готово", state: "streaming" });
    timeline.upsert(done);
    flushSync();
    expect(liveMode(target)).toEqual({ window: true, line: true });

    if (done.kind === "block") done.block.state = "final";
    slot.stop();
    flushSync();
    expect(liveMode(target)).toEqual({ window: false, line: false });
    expect(detached(target)).toBeNull();
  });

  const cardLine = (target: HTMLElement) => card(target)?.querySelector("[data-working]")?.textContent ?? "";

  it("a_running_tool_names_the_tool_and_counts_from_the_phase_start_like_the_header", () => {
    // Шапка берёт `summary || tool_name` и начало фазы: строка карточки говорит то же самое.
    const slot = liveSlot({ input: INPUT, command: COMMAND, phase: "running_tool", tool: "Bash", summary: "cargo test", since: "2026-11-25T09:58:00.000Z", phaseSince: "2026-11-25T09:59:30.000Z" });
    const { target } = render([block("text", { text: "Запускаю." }), block("tool_call", { tool: "Bash" })], slot);

    expect(cardLine(target)).toContain(liveLineText("tool", 30_000, "cargo test").split(" · ")[0]!);
    expect(cardLine(target)).toContain(liveLineText("tool", 30_000, "cargo test").split(" · ")[1]!);
    expect(cardLine(target)).not.toContain("Генерирует");
  });

  it("streaming_reasoning_still_shows_its_own_phase_while_the_hub_says_generating", () => {
    const slot = liveSlot({ input: INPUT, command: COMMAND, phase: "generating" });
    const { target } = render([block("text", { text: "Посмотрю." }), block("reasoning", { text: "Думаю над схемой", state: "streaming" })], slot);

    expect(cardLine(target)).toContain("Рассуждает");
  });
});
