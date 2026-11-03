import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import TextBlock from "#lib/timeline/TextBlock.svelte";
import { BlockModel } from "#lib/stores/blocks.js";

describe("TextBlock", () => {
  beforeEach(() => {
    // Кадр не наступает: всё видимое должно появиться без requestAnimationFrame.
    vi.stubGlobal("requestAnimationFrame", () => 0);
    vi.stubGlobal("cancelAnimationFrame", () => undefined);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    document.body.textContent = "";
  });

  it("streaming_delta_is_in_the_dom_without_waiting_for_a_frame", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const block = new BlockModel("b-1", "text");
    block.text = "первое слово";
    const view = mount(TextBlock, { target, props: { block } });
    flushSync();
    expect(target.textContent).toContain("первое слово");

    block.text = "первое слово и второе слово";
    flushSync();
    expect(target.textContent).toContain("первое слово и второе слово");
    unmount(view);
  });

  it("long_plain_tail_grows_by_appending_to_one_text_node", () => {
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

    block.text += "(хвост)";
    flushSync();
    expect(tail.firstElementChild!.firstChild).toBe(node);
    expect(node.data.endsWith("(хвост)")).toBe(true);
    expect(target.textContent).toContain("<b>не разметка</b>");
    unmount(view);
  });

  it("streaming_never_shows_a_shorter_prefix_than_received", () => {    const target = document.createElement("div");
    document.body.appendChild(target);
    const block = new BlockModel("b-2", "text");
    const view = mount(TextBlock, { target, props: { block } });
    const seen: string[] = [];
    const observer = new MutationObserver(() => seen.push(target.textContent ?? ""));
    observer.observe(target, { childList: true, subtree: true, characterData: true });
    for (const text of ["раз", "раз два", "раз два три"]) {
      block.text = text;
      flushSync();
    }
    observer.disconnect();
    expect(seen.every((text) => !text.trim() || ["раз", "раз два", "раз два три"].includes(text.trim()))).toBe(true);
    expect(target.textContent?.trim()).toBe("раз два три");
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
