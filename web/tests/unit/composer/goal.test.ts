import { beforeEach, describe, expect, it, vi } from "vitest";
import { SvelteMap } from "svelte/reactivity";
import { NetworkError } from "#lib/api/http.js";

const setGoal = vi.hoisted(() => vi.fn());
vi.mock("#lib/api/client.js", () => ({ setGoal }));

import { ComposerModel, type ComposerDeps } from "#lib/composer/model.svelte.js";

interface HubDraft {
  text: string;
  attachment_ids: string[];
  revision: number;
  device_id?: string;
}

/** Общий черновик, как его хранит хаб, и локальный буфер устройства. */
function fakes() {
  let hub: HubDraft | null = null;
  const local = new Map<string, Record<string, unknown>>();
  const deps = {
    conversationId: "c1",
    chat: { inputs: new SvelteMap(), admitLocal() {}, discardLocal() {}, queue: { queued: 0, run_slot: null }, activity: { state: "idle" } },
    ops: {},
    drafts: {
      set: (id: string, patch: Record<string, unknown>) => local.set(id, { ...(local.get(id) ?? {}), ...patch }),
      load: async (id: string) => local.get(id) ?? null,
      clear: async (id: string) => { local.delete(id); },
    },
    uploads: { list: () => [], restore() {}, cancel: async () => undefined },
    config: () => ({}),
    canSend: async () => "ok" as const,
    connection: () => ({ phase: "live" }),
    deviceId: "device-a",
    serverDrafts: {
      get: async () => hub,
      put: async (text: string, attachmentIds: string[], base: number) => {
        hub = text === "" && attachmentIds.length === 0 ? null : { text, attachment_ids: attachmentIds, revision: base + 1, device_id: "device-a" };
        return hub ?? { text: "", attachment_ids: [], revision: base + 1, device_id: "device-a" };
      },
    },
    now: () => 0,
    schedule: () => () => undefined,
    online: () => true,
  } as unknown as ComposerDeps;
  return { deps, hubDraft: () => hub };
}

async function typedGoal(deps: ComposerDeps): Promise<ComposerModel> {
  const model = new ComposerModel(deps);
  await model.startDraft(null);
  model.text = "сделай релиз 1.52";
  model.typed();
  // Дебаунс сработал: текст цели лежит в общем черновике.
  await model.flushDraft();
  return model;
}

describe("composer/send as goal", () => {
  beforeEach(() => {
    setGoal.mockReset();
  });

  it("clears the shared draft, so the goal text does not come back as unsent", async () => {
    setGoal.mockResolvedValue({ data: {} });
    const { deps, hubDraft } = fakes();
    const model = await typedGoal(deps);
    expect(hubDraft()?.text).toBe("сделай релиз 1.52");

    await model.sendAsGoal();

    expect(setGoal).toHaveBeenCalledWith("c1", { action: "set", objective: "сделай релиз 1.52" }, expect.any(String));
    expect(model.text).toBe("");
    expect(hubDraft()).toBeNull();

    // Повторное открытие чата, перезагрузка или другое устройство.
    await model.leaveDraft();
    const next = new ComposerModel(deps);
    await next.startDraft(hubDraft());
    expect(next.text).toBe("");
  });

  it("keeps the text and shows why the goal was not set", async () => {
    setGoal.mockRejectedValue(new NetworkError("offline"));
    const { deps, hubDraft } = fakes();
    const model = await typedGoal(deps);

    await expect(model.sendAsGoal()).resolves.toBeUndefined();

    expect(model.text).toBe("сделай релиз 1.52");
    expect(model.errorMessage).toBeTruthy();
    expect(model.sending).toBe(false);
    expect(hubDraft()?.text).toBe("сделай релиз 1.52");
  });
});
