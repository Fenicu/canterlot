import { describe, expect, it } from "vitest";
import {
  DEFAULT_PERMISSION_MODE,
  PERMISSION_MODES,
  availabilityLabel,
  blockEndLabel,
  diagnosticKindLabel,
  effortLabel,
  executionOutcomeLabel,
  executionStateLabel,
  permissionHint,
  permissionLabel,
  permissionValue,
  providerLabel,
  rejectionLabel,
  scopeStatusLabel,
  settingKeyLabel,
} from "#lib/format/labels.js";

const RAW = /^[a-z0-9]+(_[a-z0-9]+)*$/;

describe("format/labels", () => {
  it("permission_modes_have_the_owner_labels", () => {
    expect(PERMISSION_MODES.map(permissionLabel)).toEqual([
      "Как в настройках CLI",
      "Спрашивать",
      "Без подтверждений",
      "Только чтение",
    ]);
  });

  it("a_session_without_a_chosen_mode_reads_as_cli_settings", () => {
    expect(permissionLabel("")).toBe("Как в настройках CLI");
    expect(permissionValue("")).toBe("owner");
    expect(permissionValue("bypass")).toBe("bypass");
  });

  it("opencode_has_the_owner_names", () => {
    expect(providerLabel("opencode")).toBe("OpenCode");
    expect(effortLabel("default")).toBe("По умолчанию");
  });

  it("new_sessions_default_to_bypass", () => {
    expect(DEFAULT_PERMISSION_MODE).toBe("bypass");
  });

  it("every_mode_has_a_hint_for_every_harness_family", () => {
    for (const provider of ["claude", "codex", "opencode"]) {
      for (const mode of PERMISSION_MODES) {
        expect(permissionHint(mode, provider)).not.toBe("");
      }
    }
    expect(permissionHint("owner", "claude")).toContain("~/.claude/settings.json");
    expect(permissionHint("owner", "claude")).toContain("dontAsk");
    expect(permissionHint("owner", "codex")).toContain("approval_policy");
    expect(permissionHint("owner", "opencode")).toContain("opencode.json");
    expect(permissionHint("read_only", "opencode")).toContain("OpenCode");
    expect(permissionHint("owner", "codex")).toContain("sandbox_mode");
    expect(permissionHint("unknown_mode", "claude")).toBe("");
  });

  it("efforts_keep_xhigh_and_max_apart", () => {
    expect(effortLabel("xhigh")).toBe("Очень высокое");
    expect(effortLabel("max")).toBe("Максимальное");
    expect(effortLabel("minimal")).toBe("Минимальное");
  });

  it("execution_outcomes_cover_what_the_worker_reports", () => {
    for (const outcome of [
      "user_stop",
      "force_stopped",
      "normal_exit",
      "start_cancelled",
      "start_failed",
      "start_rejected",
      "invalid_instructions",
      "auth_required",
      "crashed",
      "owner_lost",
    ]) {
      expect(executionOutcomeLabel(outcome)).not.toMatch(RAW);
      expect(executionOutcomeLabel(outcome)).not.toBe(
        executionOutcomeLabel("something_new"),
      );
    }
  });

  it("unknown_values_get_a_neutral_russian_fallback_never_the_raw_string", () => {
    const raw = "brand_new_value";
    for (const present of [
      permissionLabel,
      effortLabel,
      executionStateLabel,
      executionOutcomeLabel,
      scopeStatusLabel,
      diagnosticKindLabel,
      blockEndLabel,
      rejectionLabel,
      settingKeyLabel,
      availabilityLabel,
      providerLabel,
    ]) {
      expect(present(raw)).not.toContain(raw);
      expect(present(raw)).toMatch(/[А-Яа-яЁё]/);
    }
  });

  it("object_prototype_keys_are_not_mistaken_for_labels", () => {
    expect(permissionLabel("constructor")).toMatch(/[А-Яа-яЁё]/);
    expect(effortLabel("toString")).toMatch(/[А-Яа-яЁё]/);
  });

  it("provider_names_stay_readable_for_the_known_two", () => {
    expect(providerLabel("claude")).toBe("Claude");
    expect(providerLabel("CODEX")).toBe("Codex");
  });

  it("machine_availability_reads_in_russian", () => {
    expect(availabilityLabel("available")).toBe("В сети");
    expect(availabilityLabel("degraded")).toBe("Связь нестабильна");
    expect(availabilityLabel("unavailable")).toBe("Не в сети");
  });
});
