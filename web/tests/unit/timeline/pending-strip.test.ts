import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import PendingStrip from "#lib/timeline/PendingStrip.svelte";
import { bindReturned } from "#lib/composer/returned.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";

vi.mock("#lib/ui/Sheet.svelte", () => ({ default: () => undefined }));
vi.mock("#lib/app/runtime.js", () => ({
  getRuntime: () => ({ journal: { ops: new Map() }, drafts: {} }),
}));

function job(id: string, state: string, text: string) {
  return {
    id,
    conversation_id: "conv-1",
    run_at: "2026-10-30T12:00:00.000Z",
    content: [{ type: "text", text }],
    attachment_ids: state === "failed" ? ["att-1"] : [],
    state,
    input_id: null,
    attempts: 1,
  };
}

function chatWith(scheduled: ReturnType<typeof job>[]): ChatStore {
  return {
    id: "conv-1",
    scheduled,
    inputs: new Map(),
    strip: () => [],
  } as unknown as ChatStore;
}

describe("timeline/PendingStrip", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function render(chat: ChatStore): HTMLElement {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(PendingStrip, { target, props: { chat } });
    flushSync();
    return target;
  }

  it("only_pending_jobs_promise_a_future_send", () => {
    const target = render(
      chatWith([
        job("a", "pending", "ждёт"),
        job("b", "fired", "ушло"),
        job("c", "cancelled", "отменено"),
      ]),
    );

    expect(target.textContent).toContain("ждёт");
    expect(target.textContent).not.toContain("ушло");
    expect(target.textContent).not.toContain("отменено");
    expect(target.querySelectorAll("button")).toHaveLength(2);
  });

  it("failed_job_is_named_and_its_text_can_be_taken_back", () => {
    const chat = chatWith([job("d", "failed", "не ушло")]);
    const target = render(chat);
    expect(target.textContent).toContain("Не отправлено");
    expect(target.textContent).toContain("не ушло");

    const take = vi.fn();
    const stop = bindReturned("conv-1", take);
    target.querySelector("button")?.click();
    stop();

    expect(take).toHaveBeenCalledWith({
      text: "не ушло",
      attachments: [{ id: "att-1" }],
    });
    expect(chat.scheduled).toEqual([]);
  });
});
