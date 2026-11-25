import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { BlockModel } from "#lib/stores/blocks.js";
import LiveWindow from "#lib/timeline/LiveWindow.svelte";
import type { LiveWindowView, WindowHead, WindowItem } from "#lib/timeline/liveWindow.js";

const NOW = 1_000_000;

function view(stream: WindowItem[], head: Partial<WindowHead> = {}): LiveWindowView {
  return {
    before: [],
    stream,
    requests: [],
    head: {
      count: stream.filter((item) => item.kind === "tool").length,
      tally: "",
      failed: 0,
      startedAt: NOW - 12_000,
      ...head,
    },
  };
}

function toolItem(over: Partial<Extract<WindowItem, { kind: "tool" }>> = {}): WindowItem {
  const block = new BlockModel("call-1", "tool_call");
  block.toolName = "Bash";
  block.state = "final";
  return {
    kind: "tool",
    key: "block:call-1",
    block,
    name: "Команда",
    detail: "cargo test",
    result: null,
    failed: false,
    running: false,
    startedAt: NOW - 1300,
    finishedAt: NOW,
    ...over,
  };
}

describe("timeline/LiveWindow", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function render(v: LiveWindowView, props: { now?: number; open?: boolean; onToggle?: () => void } = {}): HTMLElement {
    if (instance) void unmount(instance);
    document.body.textContent = "";
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(LiveWindow, {
      target,
      props: { view: v, now: props.now ?? NOW, open: props.open ?? false, onToggle: props.onToggle ?? (() => undefined) },
    });
    flushSync();
    return target;
  }

  it("head_shows_the_count_tally_and_time_from_the_first_step", () => {
    const v = view([], { count: 12, tally: "Правка 7 · Команда 5", startedAt: NOW - 65_000 });
    const head = render(v).querySelector('[data-role="live-window-head"]')!;

    expect(head.textContent).toContain("12 шагов");
    expect(head.textContent).toContain("Правка 7 · Команда 5");
    expect(head.textContent).toContain("1 мин 5 с");
  });

  it("head_shows_the_error_count_instead_of_the_clock", () => {
    const v = view([], { count: 3, failed: 1, startedAt: NOW - 65_000 });
    const head = render(v).querySelector('[data-role="live-window-head"]')!;

    expect(head.textContent).toContain("Ошибка 1");
    expect(head.textContent).not.toContain("1 мин 5 с");
  });

  it("head_clock_follows_the_now_prop", () => {
    const v = view([], { count: 1, startedAt: NOW - 65_000 });
    expect(render(v, { now: NOW }).textContent).toContain("1 мин 5 с");
    expect(render(v, { now: NOW + 10_000 }).textContent).toContain("1 мин 15 с");
  });

  it("a_running_tool_shows_the_spinner_and_its_elapsed_time", () => {
    const target = render(view([toolItem({ running: true, finishedAt: null, startedAt: NOW - 3000 })]));
    const line = target.querySelector(".tool")!;

    expect(line.textContent).toContain("3 с");
    expect(line.querySelector(".spin")).not.toBeNull();
  });

  it("a_done_tool_shows_its_duration_and_an_error_says_so", () => {
    const done = toolItem({ key: "block:done", startedAt: NOW - 1300, finishedAt: NOW });
    const failed = toolItem({ key: "block:failed", failed: true, startedAt: NOW - 2000, finishedAt: NOW });
    const target = render(view([done, failed]));
    const lines = target.querySelectorAll(".tool");

    expect(lines[0]?.textContent).toContain("1 с");
    expect(lines[0]?.querySelector(".spin")).toBeNull();
    expect(lines[1]?.textContent).toContain("Ошибка");
    expect(lines[1]?.textContent).not.toContain("2 с");
    expect(lines[1]?.hasAttribute("data-failed")).toBe(true);
  });

  it("shows_only_the_last_four_lines_of_the_result", () => {
    const result = new BlockModel("result-1", "tool_result");
    result.text = "один\nдва\nтри\nчетыре\nпять\nшесть";
    const target = render(view([toolItem({ result })]));
    const out = target.querySelector(".out")!;

    expect(out.textContent).not.toContain("один");
    expect(out.textContent).not.toContain("два");
    for (const line of ["три", "четыре", "пять", "шесть"]) expect(out.textContent).toContain(line);
  });

  it("streaming_reasoning_shows_the_caret_and_finished_thought_does_not", () => {
    const streaming = new BlockModel("r1", "reasoning");
    streaming.text = "думаю";
    streaming.state = "streaming";
    const done = new BlockModel("r2", "reasoning");
    done.text = "подумал";
    done.state = "final";
    const target = render(
      view([
        { kind: "reasoning", key: "block:r1", block: streaming },
        { kind: "reasoning", key: "block:r2", block: done },
      ]),
    );
    const items = target.querySelectorAll('[data-role="live-reasoning"]');

    expect(items[0]?.querySelector(".caret")).not.toBeNull();
    expect(items[1]?.querySelector(".caret")).toBeNull();
  });

  it("a_tap_on_the_head_and_on_the_window_asks_for_the_full_list", () => {
    const onToggle = vi.fn();
    const target = render(view([toolItem()]), { onToggle });
    const head = target.querySelector('[data-role="live-window-head"] button')!;
    head.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();
    expect(onToggle).toHaveBeenCalledTimes(1);

    (target.querySelector('[data-role="live-window-body"]') as HTMLElement).click();
    flushSync();
    expect(onToggle).toHaveBeenCalledTimes(2);
  });

  it("keeps_only_the_head_when_the_full_list_is_open", () => {
    const target = render(view([toolItem()]), { open: true });

    expect(target.querySelector('[data-role="live-window-body"]')).toBeNull();
    expect(target.querySelector('[data-role="live-window-head"] button')?.getAttribute("aria-expanded")).toBe("true");
  });
});
