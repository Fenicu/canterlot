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
  prefillWorkdir,
  soleMachine,
  type Field,
} from "#lib/screens/sessions/newSessionModel.js";
import { openDb, purgeAllStores } from "#lib/storage/idb.js";
import type {
  MachineRecord,
  MachinesStore,
} from "#lib/stores/machines.svelte.js";

vi.mock("#lib/api/client.js", () => ({
  recentWorkdirs: vi.fn(async () => ({
    data: { workdirs: ["/srv/canterlot", "/srv/notes", "/srv/third", "/srv/fourth"] },
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
  it("new_session_takes_the_machine_only_when_it_is_the_only_one", () => {
    expect(soleMachine([])).toBeNull();
    expect(soleMachine([machine()])).toBe("m1");
    expect(soleMachine([machine(), machine({ id: "m2" })])).toBeNull();
  });

  it("new_session_prefills_home_and_follows_the_machine_until_the_owner_types", () => {
    expect(prefillWorkdir("", "", "/home/owner")).toBe("/home/owner");
    expect(prefillWorkdir("/home/owner", "/home/owner", "/home/other")).toBe(
      "/home/other",
    );
    expect(prefillWorkdir("/srv/typed", "/home/owner", "/home/other")).toBe(
      "/srv/typed",
    );
  });

  it("new_session_stays_empty_for_a_worker_without_a_home", () => {
    expect(prefillWorkdir("", "", "")).toBe("");
    expect(prefillWorkdir("/home/owner", "/home/owner", "")).toBe("");
    expect(prefillWorkdir("/srv/typed", "/home/owner", "")).toBe("/srv/typed");
  });

  it("new_session_model_accepts_any_absolute_workdir", () => {
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.machineId = "m1";
    model.workdir = "relative";
    expect(model.errors.workdir).toBe("Укажите абсолютный путь");
    model.workdir = "/srv/../etc";
    expect(model.errors.workdir).toContain("..");
    // Политика корней осталась на хабе: клиент принимает любой абсолютный путь.
    model.workdir = "/srv-evil/canterlot";
    expect(model.errors.workdir).toBeUndefined();
    model.workdir = "/etc";
    expect(model.errors.workdir).toBeUndefined();
    model.workdir = "/srv/canterlot";
    expect(model.errors.workdir).toBeUndefined();
  });

  it("new_session_lists_last_three_recent_workdirs_from_the_hub", async () => {
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.machineId = "m1";
    await model.refreshRecent();
    expect(recentWorkdirs).toHaveBeenCalledWith("m1");
    expect(model.recent).toEqual(["/srv/canterlot", "/srv/notes", "/srv/third"]);
  });

  it("new_session_submits_a_typed_path_outside_reported_roots", async () => {
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.machineId = "m1";
    model.workdir = "/tmp/typed-by-hand";
    model.harness = "claude";
    model.model = "opus";
    model.effort = "medium";
    expect(model.canSubmit).toEqual({ ok: true });
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

  it("new_session_refusal_lasts_only_until_the_form_changes", async () => {
    const refuse = (code: string) =>
      vi.fn(async () => {
        throw new ApiError(
          422,
          code as never,
          "нет",
          "browser",
          "never",
          "00000000-0000-4000-8000-0000000000e1",
        );
      });
    const journal = ops();
    journal.run = refuse("workdir_not_allowed");
    const model = new NewSessionModel(store([machine()]), config, journal);
    ready(model);
    await expect(model.submit()).rejects.toBeInstanceOf(ApiError);
    expect(model.canSubmit).toEqual({
      ok: false,
      reason: "Рабочая папка не разрешена",
    });

    model.workdir = "/srv/other";
    expect(model.canSubmit).toEqual({ ok: true });
    expect(model.errors.workdir).toBeUndefined();

    journal.run = refuse("capability_missing");
    await expect(model.submit()).rejects.toBeInstanceOf(ApiError);
    expect(model.canSubmit.ok).toBe(false);
    model.profile = "owner";
    expect(model.canSubmit).toEqual({ ok: true });
  });

  it("new_session_ignores_recent_folders_of_a_machine_no_longer_selected", async () => {
    let answer!: (value: { data: { workdirs: string[] } }) => void;
    vi.mocked(recentWorkdirs).mockImplementationOnce(
      () => new Promise((resolve) => (answer = resolve)) as never,
    );
    const model = new NewSessionModel(
      store([machine(), machine({ id: "m2" })]),
      config,
      ops(),
    );
    model.machineId = "m1";
    const slow = model.refreshRecent();
    model.machineId = "m2";
    await model.refreshRecent();
    const fresh = model.recent;

    answer({ data: { workdirs: ["/from/m1"] } });
    await slow;

    expect(model.recent).toEqual(fresh);
    expect(model.recent).not.toContain("/from/m1");
  });

  it("new_session_recent_folders_failure_leaves_the_form_usable", async () => {
    vi.mocked(recentWorkdirs).mockRejectedValueOnce(new Error("down"));
    const model = new NewSessionModel(store([machine()]), config, ops());
    model.recent = ["/stale"];
    model.machineId = "m1";

    await expect(model.refreshRecent()).resolves.toBeUndefined();

    expect(model.recent).toEqual([]);
  });

  it("new_session_sends_the_path_without_surrounding_spaces", async () => {
    const journal = ops();
    const model = new NewSessionModel(store([machine()]), config, journal);
    ready(model);
    model.workdir = " /srv/canterlot ";

    expect(model.canSubmit).toEqual({ ok: true });
    await model.submit();

    expect(journal.begin).toHaveBeenCalledWith(
      "create_conversation",
      expect.objectContaining({ workdir: "/srv/canterlot" }),
      expect.anything(),
    );
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
