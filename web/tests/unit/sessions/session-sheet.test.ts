import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { getConversation, setCrunch } from "#lib/api/client.js";
import SessionSheet from "#lib/screens/sessions/SessionSheet.svelte";
import type { ListRow } from "#lib/stores/list.svelte.js";

vi.mock("#lib/ui/Sheet.svelte", () => ({ default: () => undefined }));
vi.mock("#lib/api/client.js", () => ({
  getConversation: vi.fn(async () => ({
    data: { crunch: { enabled: false, resume_text: "мой текст" } },
  })),
  setCrunch: vi.fn(async () => ({ data: {} })),
  patchConversation: vi.fn(),
  patchSettings: vi.fn(),
  setConversationGroups: vi.fn(),
}));

const row = {
  id: "conv-1",
  title: "Сессия",
  pinned: false,
  color: null,
  groups: [],
  model: "opus",
  effort: "medium",
  permission_mode: "default",
  crunch: { enabled: false },
} as unknown as ListRow;

describe("sessions/SessionSheet", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
    vi.clearAllMocks();
  });

  it("crunch_toggle_saves_at_once_with_the_stored_text", async () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(SessionSheet, {
      target,
      props: { item: row, groups: [], touch: false, onclose: () => undefined },
    });
    flushSync();
    const crunch = [
      ...target.querySelectorAll<HTMLInputElement>('input[role="switch"]'),
    ].at(-1)!;

    crunch.click();
    flushSync();
    await vi.waitFor(() => expect(setCrunch).toHaveBeenCalledTimes(1));
    expect(getConversation).toHaveBeenCalledWith("conv-1");
    expect(setCrunch).toHaveBeenLastCalledWith(
      "conv-1",
      { enabled: true, resume_text: "мой текст" },
      expect.any(String),
    );

    crunch.click();
    flushSync();
    await vi.waitFor(() => expect(setCrunch).toHaveBeenCalledTimes(2));
    expect(setCrunch).toHaveBeenLastCalledWith(
      "conv-1",
      { enabled: false, resume_text: "мой текст" },
      expect.any(String),
    );
  });
});
