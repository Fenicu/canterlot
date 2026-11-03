import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import TextBlock from "#lib/timeline/TextBlock.svelte";
import { BlockModel } from "#lib/stores/blocks.js";

/** Кадры под рукой: контроллер проявляет текст только по requestAnimationFrame. */
function ticker() {
  let queue: FrameRequestCallback[] = [];
  let now = 0;
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    queue.push(callback);
    return queue.length;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {
    queue = [];
  });
  const clock = vi
    .spyOn(performance, "now")
    .mockImplementation(() => now);
  return {
    advance(ms = 16): void {
      now += ms;
      const pending = queue;
      queue = [];
      for (const callback of pending) callback(now);
      flushSync();
    },
    dispose(): void {
      clock.mockRestore();
    },
  };
}

describe("TextBlock", () => {
  let frames: ReturnType<typeof ticker>;

  beforeEach(() => {
    // jsdom не применяет таблицы стилей: множитель скорости задаётся вручную.
    document.documentElement.style.setProperty("--motion-scale", "1");
    frames = ticker();
  });

  afterEach(() => {
    frames.dispose();
    vi.unstubAllGlobals();
    document.documentElement.style.removeProperty("--motion-scale");
    document.body.textContent = "";
  });

  it("streaming_text_already_received_is_visible_at_mount_and_grows_in_frames", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const block = new BlockModel("b-1", "text");
    block.text = "первое слово";
    const view = mount(TextBlock, { target, props: { block } });
    flushSync();
    expect(target.textContent).toContain("первое слово");

    block.text = "первое слово и второе слово";
    flushSync();
    expect(target.textContent).toContain("первое слово");
    for (let step = 0; step < 50; step++) frames.advance(16);
    expect(target.textContent).toContain("первое слово и второе слово");
    unmount(view);
  });

  it("long_plain_tail_grows_without_rebuilding_the_shown_node", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const block = new BlockModel("b-3", "text");
    block.text = "<b>не разметка</b>" + "x".repeat(9000);
    const view = mount(TextBlock, { target, props: { block } });
    flushSync();
    const tail = target.querySelector(".tail")!;
    const node = tail.firstElementChild!.firstChild as Text;
    expect(node.nodeType).toBe(Node.TEXT_NODE);
    expect(tail.querySelector("b")).toBeNull();

    block.text += " (хвост)";
    flushSync();
    for (let step = 0; step < 50; step++) frames.advance(16);
    expect(tail.firstElementChild!.firstChild).toBe(node);
    expect(target.textContent).toContain("(хвост)");
    expect(tail.querySelector(".reveal-word")?.textContent).toBe("(хвост)");
    unmount(view);
  });

  it("streaming_never_shows_a_shorter_prefix_than_received", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const block = new BlockModel("b-2", "text");
    const view = mount(TextBlock, { target, props: { block } });
    const seen: string[] = [];
    let received = "";
    const sample = () => {
      const value = (target.textContent ?? "").trim();
      seen.push(value);
      expect(received.startsWith(value), `${value} / ${received}`).toBe(true);
    };
    for (const text of ["раз", "раз два", "раз два три", "раз два три четыре"]) {
      block.text = text;
      received = text;
      flushSync();
      sample();
      frames.advance(20);
      sample();
    }
    for (let step = 0; step < 50; step++) {
      frames.advance(20);
      sample();
    }
    expect(target.textContent?.trim()).toBe("раз два три четыре");
    for (let index = 1; index < seen.length; index++)
      expect(seen[index]!.length).toBeGreaterThanOrEqual(
        seen[index - 1]!.length,
      );
    unmount(view);
  });

  it("quote_copy_button_appears_only_for_a_final_block", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const block = new BlockModel("b-q", "text");
    block.text = "> цитата";
    const view = mount(TextBlock, { target, props: { block } });
    flushSync();
    expect(target.querySelector('button[aria-label="Скопировать цитату"]')).toBeNull();

    block.state = "final";
    flushSync();
    const button = target.querySelector('button[aria-label="Скопировать цитату"]');
    expect(button).not.toBeNull();
    unmount(view);
  });
});
