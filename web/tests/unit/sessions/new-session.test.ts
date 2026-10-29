import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { recentWorkdirs } from "#lib/api/client.js";
import { ApiError, NetworkError } from "#lib/api/http.js";
import type { components } from "#lib/api/generated/openapi.js";
import {
  closeOpenJournals,
  OpJournal,
  type JournalEnv,
  type Op,
} from "#lib/ops/journal.js";
import {
  NewSessionModel,
  type Field,
} from "#lib/screens/sessions/newSessionModel.js";
import { openDb, purgeAllStores } from "#lib/storage/idb.js";
import type {
  MachineRecord,
  MachinesStore,
} from "#lib/stores/machines.svelte.js";

vi.mock("#lib/api/client.js", () => ({
  recentWorkdirs: vi.fn(async () => ({
    data: { workdirs: ["/srv/canterlot", "/srv/notes"] },
  })),
}));

const config = {
  defaults: { crunch_resume_text: "продолжи с места остановки" },
} as components["schemas"]["ConfigView"];

function machine(patch: Partial<MachineRecord> = {}): MachineRecord {
  return {
    id: "m1",
    name: "workstation",
    availability: "ready",
    os: "linux",
    warnings: [],
    metrics: null,
    capabilities: ["pilot_v1"],
    allowed_roots: ["/srv"],
    harnesses: [
      {
        id: "claude",
        title: "Claude Code",
        provider: "claude",
        models: [
          { id: "opus", title: "opus", efforts: ["low", "medium"] },
          { id: "haiku", title: "haiku", efforts: ["low"] },
        ],
        permission_modes: ["default"],
      },
    ],
    profiles: [],
    ...patch,
  };
}

function store(items: MachineRecord[]): MachinesStore {
  return {
    items: new Map(items.map((item) => [item.id, item])),
  } as unknown as MachinesStore;
}

function ops(): OpJournal {
  return {
    begin: vi.fn(
      async (_kind: string, body: unknown, opts: { key?: string }) => ({
        key: opts.key || "key-1",
        kind: "create_conversation",
        body,
        state: "saving",
        attempts: 0,
        createdAt: 0,
      }),
    ),
    run: vi.fn(async (op: Op) => {
      op.result = { conversation_id: "conv-1" };
      op.state = "committed";
    }),
  } as unknown as OpJournal;
}

function ready(model: NewSessionModel) {
  model.machineId = "m1";
  model.workdir = "/srv/canterlot";
  model.harness = "claude";
  model.model = "opus";
  model.effort = "medium";
}

describe("sessions/new", () => {
  it("new_session_model_validates_workdir_under_reported_roots", () => {
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.machineId = "m1";
    model.workdir = "relative";
    expect(model.errors.workdir).toBe("Укажите абсолютный путь");
    model.workdir = "/srv/../etc";
    expect(model.errors.workdir).toContain("..");
    model.workdir = "/srv-evil/canterlot";
    expect(model.errors.workdir).toBe("Папка вне известных корней машины");
    model.workdir = "/srv/canterlot";
    expect(model.errors.workdir).toBeUndefined();
  });

  it("new_session_lists_recent_workdirs_from_the_hub", async () => {
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.machineId = "m1";
    await model.refreshRecent();
    expect(recentWorkdirs).toHaveBeenCalledWith("m1");
    expect(model.recent).toEqual(["/srv/canterlot", "/srv/notes"]);
  });

  it("new_session_effort_list_follows_the_selected_model", () => {
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.machineId = "m1";
    model.harness = "claude";
    model.model = "opus";
    expect(model.efforts).toEqual(["low", "medium"]);
    model.model = "haiku";
    expect(model.efforts).toEqual(["low"]);
    model.workdir = "/srv/canterlot";
    model.effort = "medium";
    expect(model.canSubmit).toEqual({ ok: false, reason: "Выберите усилие" });
  });

  it("new_session_maps_every_hub_create_code_to_its_field", async () => {
    const codes: [string, Field][] = [
      ["machine_not_found", "machine"],
      ["capability_missing", "machine"],
      ["harness_unknown", "harness"],
      ["model_unknown", "model"],
      ["effort_not_allowed", "effort"],
      ["permission_mode_unknown", "permission"],
      ["profile_unknown", "profile"],
      ["workdir_not_allowed", "workdir"],
      ["group_not_found", "group"],
    ];
    for (const [code, field] of codes) {
      const journal = ops();
      journal.run = vi.fn(async () => {
        throw new ApiError(
          409,
          code as never,
          "нет",
          "delivery",
          "never",
          "00000000-0000-4000-8000-0000000000e1",
        );
      });
      const model = new NewSessionModel(store([machine()]), config, journal);
      ready(model);
      await expect(model.submit()).rejects.toBeInstanceOf(ApiError);
      expect(model.errors[field]?.length).toBeGreaterThan(0);
    }
  });

  it("new_session_disables_unavailable_machine_with_a_reason", () => {
    const down = new NewSessionModel(
      store([machine({ availability: "degraded" })]),
      config,
      ops(),
    );
    down.machineId = "m1";
    expect(down.errors.machine).toBe("Машина недоступна: degraded");
    const bare = new NewSessionModel(
      store([machine({ capabilities: [] })]),
      config,
      ops(),
    );
    bare.machineId = "m1";
    expect(bare.errors.machine).toBe("Машина не готова к пилоту");
  });

  it("create_session_retry_reuses_the_key_and_creates_one_conversation", async () => {
    closeOpenJournals();
    await purgeAllStores();
    const db = await openDb("new-session");
    const env: JournalEnv = {
      now: () => 0,
      online: () => true,
      authExpired: () => false,
      schedule: (fn, ms) => {
        setTimeout(fn, ms);
        return () => undefined;
      },
    };
    let hits = 0;
    const keys: string[] = [];
    const journal = new OpJournal(
      db,
      async (op) => {
        hits += 1;
        keys.push(op.key);
        if (hits === 1) throw new NetworkError("send_failed");
        return { conversation_id: "conv-1" };
      },
      env,
    );
    const model = new NewSessionModel(store([machine()]), config, journal);
    ready(model);
    const created = await model.submit();
    expect(created.conversationId).toBe("conv-1");
    expect(hits).toBe(2);
    expect(keys[0]).toBe(keys[1]);
    expect(new Set(keys).size).toBe(1);
  });
});
