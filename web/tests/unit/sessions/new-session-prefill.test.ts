import { describe, expect, it } from "vitest";
import type { MachineRecord } from "#lib/stores/machines.svelte.js";
import {
  latestSession,
  prefillFromSession,
  shortenHomePath,
  type PrefillSession,
} from "#lib/screens/sessions/newSessionPrefill.js";

function machine(patch: Partial<MachineRecord> = {}): MachineRecord {
  return {
    id: "m1",
    name: "workstation",
    availability: "ready",
    os: "linux",
    warnings: [],
    metrics: null,
    capabilities: ["pilot_v1"],
    harnesses: [
      {
        id: "claude",
        title: "Claude Code",
        provider: "claude",
        models: [{ id: "opus", title: "opus-5.5", efforts: ["medium", "high"] }],
        permission_modes: ["owner"],
      },
    ],
    ...patch,
  };
}

function session(patch: Partial<PrefillSession> = {}): PrefillSession {
  return {
    machine: { id: "m1" },
    harness: { id: "claude" },
    workdir: "/srv/canterlot",
    model: "opus",
    effort: "medium",
    created_at: "2026-11-01T10:00:00.000Z",
    groups: [],
    ...patch,
  };
}

describe("sessions/new prefill", () => {
  it("new_session_prefill_takes_the_last_created_session", () => {
    const older = session({
      created_at: "2026-10-01T10:00:00.000Z",
      workdir: "/srv/older",
    });
    const newer = session({
      created_at: "2026-11-02T10:00:00.000Z",
      workdir: "/srv/newer",
    });

    expect(latestSession([newer, older])).toBe(newer);
    expect(prefillFromSession(latestSession([older, newer]), [machine()])).toEqual({
      machineId: "m1",
      harness: "claude",
      workdir: "/srv/newer",
      modelId: "opus",
      effort: "medium",
    });
  });

  it("new_session_prefill_takes_the_last_session_of_the_open_group", () => {
    const grouped = session({
      created_at: "2026-10-05T10:00:00.000Z",
      groups: ["g1"],
      workdir: "/srv/grouped",
    });
    const newer = session({
      created_at: "2026-11-05T10:00:00.000Z",
      groups: ["g2"],
      workdir: "/srv/other",
    });

    expect(latestSession([grouped, newer], "g1")).toBe(grouped);
    expect(latestSession([grouped, newer])).toBe(newer);
    expect(latestSession([grouped, newer], "missing")).toBeNull();
  });

  it("new_session_prefill_ignores_a_session_started_by_an_agent", () => {
    const manual = session({
      created_at: "2026-11-01T10:00:00.000Z",
      workdir: "/srv/manual",
    });
    const delegate = session({
      created_at: "2026-11-02T10:00:00.000Z",
      workdir: "/srv/delegate",
      parent_id: "orchestrator",
    });

    expect(latestSession([delegate, manual])).toBe(manual);
    expect(
      prefillFromSession(latestSession([manual, delegate]), [machine()])?.workdir,
    ).toBe("/srv/manual");
  });

  it("new_session_prefill_ignores_an_agent_session_of_the_open_group", () => {
    const grouped = session({
      created_at: "2026-10-05T10:00:00.000Z",
      groups: ["g1"],
      workdir: "/srv/grouped",
    });
    const delegate = session({
      created_at: "2026-11-05T10:00:00.000Z",
      groups: ["g1"],
      workdir: "/srv/delegate",
      parent_id: "orchestrator",
    });

    expect(latestSession([grouped, delegate], "g1")).toBe(grouped);
  });

  it("new_session_prefill_stays_empty_when_only_agents_started_sessions", () => {
    const delegate = session({ parent_id: "orchestrator" });

    expect(latestSession([delegate])).toBeNull();
    expect(prefillFromSession(latestSession([delegate]), [machine()])).toBeNull();
  });

  it("new_session_prefill_leaves_an_offline_or_unknown_machine_empty", () => {
    for (const availability of ["unavailable", "degraded"]) {
      const prefill = prefillFromSession(session(), [
        machine({ availability }),
      ]);
      expect(prefill?.machineId).toBe("");
      expect(prefill?.harness).toBe("");
      expect(prefill?.modelId).toBe("");
      expect(prefill?.effort).toBe("");
    }

    expect(prefillFromSession(session(), [])?.machineId).toBe("");
  });

  it("new_session_prefill_drops_a_model_that_left_the_catalog", () => {
    const prefill = prefillFromSession(session({ model: "removed-model" }), [
      machine(),
    ]);

    expect(prefill).toEqual({
      machineId: "m1",
      harness: "claude",
      workdir: "/srv/canterlot",
      modelId: "",
      effort: "",
    });
  });

  it("new_session_prefill_drops_an_effort_the_model_no_longer_takes", () => {
    const prefill = prefillFromSession(session({ effort: "max" }), [machine()]);

    expect(prefill?.effort).toBe("");
    expect(prefill?.modelId).toBe("opus");
  });

  it("new_session_prefill_drops_a_harness_the_machine_no_longer_lists", () => {
    const prefill = prefillFromSession(session({ harness: { id: "codex" } }), [
      machine(),
    ]);

    expect(prefill?.harness).toBe("");
    expect(prefill?.modelId).toBe("");
  });

  it("new_session_prefill_stays_empty_without_previous_sessions", () => {
    expect(latestSession([])).toBeNull();
    expect(prefillFromSession(null, [machine()])).toBeNull();
  });

  it("new_session_summary_shortens_the_home_folder", () => {
    expect(shortenHomePath("/home/owner/projects/canterlot", "/home/owner")).toBe(
      "~/projects/canterlot",
    );
    expect(shortenHomePath("/home/owner", "/home/owner")).toBe("~");
    expect(shortenHomePath("/srv/canterlot", "/home/owner")).toBe("/srv/canterlot");
    expect(shortenHomePath("/home/owner2/canterlot", "/home/owner")).toBe(
      "/home/owner2/canterlot",
    );
    expect(shortenHomePath("/srv/canterlot", "")).toBe("/srv/canterlot");
  });
});
