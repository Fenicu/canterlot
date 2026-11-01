import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import UserBubble from "#lib/timeline/UserBubble.svelte";
import { freshInput } from "#lib/stores/inputs.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";

vi.mock("#lib/ui/Sheet.svelte", () => ({ default: () => undefined }));
const ops = new Map<string, { state: string }>();
const retryNow = vi.fn();
vi.mock("#lib/app/runtime.js", () => ({
  getRuntime: () => ({ journal: { ops, retryNow }, drafts: {} }),
}));

describe("timeline/UserBubble optimistic", () => {
  let instance: ReturnType<typeof mount> | undefined;
  beforeEach(() => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query.includes("reduce"),
      media: query,
      addEventListener() {},
      removeEventListener() {},
    }));
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
    ops.clear();
    retryNow.mockClear();
  });

  function render(state: "saving" | "queued" | "on_machine") {
    const input = freshInput("k1", "привет", 1);
    input.state = state;
    input.optimistic = true;
    input.placement = "feed";
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(UserBubble, { target, props: { input, chat: { id: "c" } as unknown as ChatStore } });
    flushSync();
    return target;
  }

  it("a_bubble_not_yet_confirmed_shows_the_first_stage_and_never_the_acceptance", () => {
    const target = render("saving");
    expect(target.textContent).toContain("Сохраняется");
    expect(target.textContent).not.toContain("Принято");
    expect(target.querySelector("[data-optimistic]")).not.toBeNull();
  });

  it("a_queued_bubble_says_so_in_words", () => {
    expect(render("queued").textContent).toContain("В очереди");
  });

  it("a_send_the_hub_has_not_confirmed_offers_a_retry_inside_the_bubble", () => {
    ops.set("k1", { state: "unknown" });
    const target = render("saving");
    expect(target.textContent).toContain("Отправка не подтверждена");
    const button = [...target.querySelectorAll("button")].find((item) => item.textContent === "Повторить сейчас");
    button?.click();
    expect(retryNow).toHaveBeenCalledWith("k1");
  });
});
