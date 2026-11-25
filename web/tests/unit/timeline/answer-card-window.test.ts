import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import type { BlockModel, BlockPage } from "#lib/stores/blocks.js";
import { BlockModel as Model } from "#lib/stores/blocks.js";
import type { RequestItem } from "#lib/stores/requests.svelte.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import AnswerCard from "#lib/timeline/AnswerCard.svelte";
import { FeedFolds } from "#lib/timeline/folds.js";
import { blockLinks } from "#lib/timeline/rows.js";
import { liveRun } from "./live-run.svelte";

const NOW = Date.parse("2026-11-25T10:00:00.000Z");
let seq = 0;

function block(
  kind: BlockModel["kind"],
  opts: { tool?: string; text?: string; summary?: string; state?: BlockModel["state"]; parent?: string } = {},
): TimelineEntry {
  seq += 1;
  const model = new Model(`b${seq}`, kind);
  model.toolName = opts.tool ?? null;
  model.parentId = opts.parent ?? null;
  model.summary = opts.summary ?? null;
  model.text = opts.text ?? "";
  model.state = opts.state ?? "final";
  model.inputCommandId = "run";
  model.createdAt = NOW + seq * 1000;
  model.startedAt = NOW + seq * 1000;
  model.finishedAt = NOW + seq * 1000;
  return { kind: "block", key: `block:${model.id}`, seq: String(seq), block: model };
}

const idOf = (entry: TimelineEntry): string => (entry.kind === "block" ? entry.block.id : "");

function call(tool = "Bash", summary = "cargo test"): TimelineEntry {
  return block("tool_call", { tool, summary });
}

function result(callEntry: TimelineEntry, text = "ok"): TimelineEntry {
  return block("tool_result", { parent: idOf(callEntry), text });
}

function request(id: string): TimelineEntry {
  const item: RequestItem = {
    id,
    conversationId: "c",
    kind: "permission",
    state: "applied",
    payload: { tool_name: "Bash" },
    winnerDecision: { behavior: "allow" },
  };
  return { kind: "request", key: `request:${id}`, seq: String(2000 + Number(id.slice(1))), request: item };
}

const load = async (): Promise<BlockPage> => ({ content: "", offset: 0, next_offset: null, rev: 0, state: "final", total_bytes: 0 });

/** Три законченных шага: в живом списке они уже свернулись бы в группу. */
function step(tool: string, summary: string): TimelineEntry[] {
  const entry = call(tool, summary);
  return [entry, result(entry)];
}

function threeSteps(): TimelineEntry[] {
  return [
    block("text", { text: "I'll start by reading the brief." }),
    ...step("Bash", "cargo test"),
    ...step("Bash", "cargo test"),
    ...step("Read", "file.ts"),
    block("reasoning", { text: "теперь проверю", state: "streaming" }),
  ];
}

const top = (all: TimelineEntry[]): TimelineEntry[] => all.filter((entry) => entry.kind !== "block" || entry.block.parentId === null);

describe("AnswerCard live window", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function render(all: TimelineEntry[], opts: { cardKey?: string; folds?: FeedFolds; activeRun?: string | null } = {}) {
    const cardKey = opts.cardKey ?? "answer:run";
    const folds = opts.folds ?? new FeedFolds();
    const run = liveRun(opts.activeRun === undefined ? "run" : opts.activeRun);
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(AnswerCard, {
      target,
      props: {
        cardKey,
        entries: top(all),
        folds,
        links: blockLinks(all),
        conversationId: "c",
        now: NOW,
        get activeRun() {
          return run.activeRun;
        },
        load,
        markable: false,
      },
    });
    flushSync();
    return { target, folds, run };
  }

  it("a_live_card_with_steps_renders_the_window_and_not_the_step_group", () => {
    const { target } = render(threeSteps());
    const head = target.querySelector('[data-role="live-window-head"]');

    expect(target.querySelector('[data-role="live-window-body"]')).not.toBeNull();
    expect(head?.textContent).toContain("3 шага");
    expect(head?.textContent).toContain("Команда 2 · Чтение 1");
    expect(target.querySelector(".steps")).toBeNull();
    expect(target.textContent).toContain("I'll start by reading the brief.");
    // В окне рассуждение идёт строкой без своей подписи: фазу называет строка хода.
    const line = target.querySelector("[data-working]");
    expect(line?.hasAttribute("data-quiet")).toBe(false);
    expect(line?.textContent).toContain("Рассуждает");
  });

  it("a_card_opened_with_the_window_choice_renders_todays_live_list", () => {
    const folds = new FeedFolds();
    folds.toggle("window:answer:run", false);
    const { target } = render(threeSteps(), { folds });

    expect(target.querySelector('[data-role="live-window-body"]')).toBeNull();
    expect(target.querySelector('[data-role="live-window-head"]')).not.toBeNull();
    expect(target.querySelector(".steps")).not.toBeNull();
    expect(target.textContent?.match(/I'll start by reading the brief\./g)).toHaveLength(1);
    // В полном списке «Рассуждает» говорит сам блок рассуждения: строка хода молчит, как и раньше.
    expect(target.querySelector("[data-working]")?.hasAttribute("data-quiet")).toBe(true);
  });

  it("the_end_of_a_turn_switches_to_the_finished_view_and_keeps_the_feed_pinned", () => {
    const beforeChange = vi.fn();
    const folds = new FeedFolds(beforeChange);
    const entries = threeSteps();
    const { target, run } = render(entries, { folds });
    expect(target.querySelector('[data-role="live-window-body"]')).not.toBeNull();

    run.stop();
    const reasoning = entries.find((entry) => entry.kind === "block" && entry.block.kind === "reasoning");
    if (reasoning?.kind === "block") reasoning.block.state = "final";
    flushSync();

    expect(beforeChange).toHaveBeenCalledWith(true, true);
    expect(target.querySelector(".steps")).not.toBeNull();
  });

  it("requests_render_below_the_window", () => {
    const spoken = call();
    const all = [block("text", { text: "Сейчас спрошу" }), spoken, result(spoken), request("r1")];
    const { target } = render(all);
    const body = target.querySelector('[data-role="live-window-body"]')!;
    const asked = [...target.querySelectorAll("button")].find((button) => button.textContent?.includes("Разрешение"));

    expect(body).not.toBeNull();
    expect(asked).toBeTruthy();
    expect(body.compareDocumentPosition(asked!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("at_the_end_of_a_turn_each_intro_key_stands_once", () => {
    const req = request("r1");
    const entries = [...threeSteps(), req];
    const intro = entries.find((entry) => entry.kind === "block" && entry.block.kind === "text")!;
    const { target, run } = render(entries);
    expect(target.querySelectorAll(`[data-key="${intro.key}"]`)).toHaveLength(1);
    expect(target.querySelectorAll(`[data-key="${req.key}"]`)).toHaveLength(1);

    run.stop();
    const reasoning = entries.find((entry) => entry.kind === "block" && entry.block.kind === "reasoning");
    if (reasoning?.kind === "block") reasoning.block.state = "final";
    flushSync();

    expect(target.querySelectorAll(`[data-key="${intro.key}"]`)).toHaveLength(1);
    expect(target.querySelectorAll(`[data-key="${req.key}"]`)).toHaveLength(1);
  });

  it("a_live_card_without_steps_stays_as_today", () => {
    const { target } = render([block("text", { text: "Отвечаю", state: "streaming" })]);

    expect(target.querySelector('[data-role="live-window"]')).toBeNull();
    expect(target.textContent).toContain("Отвечаю");
    // Без окна поведение прежнее: здесь идёт текст, и строка хода называет свою фазу.
    expect(target.querySelector("[data-working]")?.hasAttribute("data-quiet")).toBe(false);
  });
});
