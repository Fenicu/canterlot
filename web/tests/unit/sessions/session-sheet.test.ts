import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { getConversation, patchSettings, setCrunch } from "#lib/api/client.js";
import SessionSheet from "#lib/screens/sessions/SessionSheet.svelte";
import type { ListRow } from "#lib/stores/list.svelte.js";
import type { MachineRecord } from "#lib/stores/machines.svelte.js";

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

  it("antigravity_model_saves_its_single_effort_with_the_model", async () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const antigravity = machine();
    instance = mount(SessionSheet, {
      target,
      props: {
        item: {
          ...row,
          harness: { id: "antigravity", title: "Antigravity", provider: "antigravity" },
          model: "gemini-3.8-flash-high",
          effort: "high",
          permission_mode: "bypass",
        } as unknown as ListRow,
        groups: [],
        machine: antigravity,
        touch: false,
        onclose: () => undefined,
      },
    });
    flushSync();
    // У Antigravity модель и её единственный effort — одно и то же решение.
    expect(target.querySelector("#sheet-effort")).toBeNull();

    const select = target.querySelector<HTMLSelectElement>("#sheet-model")!;
    select.value = "gemini-3.8-flash-low";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    await vi.waitFor(() => expect(patchSettings).toHaveBeenCalledTimes(1));
    expect(patchSettings).toHaveBeenCalledWith(
      "conv-1",
      { model: "gemini-3.8-flash-low", effort: "low" },
      expect.any(String),
    );
  });
});

function machine(): MachineRecord {
  return {
    id: "m1",
    name: "workstation",
    availability: "ready",
    os: "linux",
    warnings: [],
    metrics: null,
    harnesses: [
      {
        id: "antigravity",
        title: "Antigravity",
        provider: "antigravity",
        models: [
          { id: "gemini-3.8-flash-high", title: "Gemini 3.8 Flash (High)", efforts: ["high"] },
          { id: "gemini-3.8-flash-low", title: "Gemini 3.8 Flash (Low)", efforts: ["low"] },
        ],
        permission_modes: ["owner", "ask", "bypass"],
      },
    ],
  };
}
