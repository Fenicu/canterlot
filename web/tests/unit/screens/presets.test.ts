import { describe, expect, it } from "vitest";
import {
  byteCounter,
  formatOrigin,
  formatOwnerMtime,
  memoryProjects,
  formatSize,
  isAgentMarked,
  ownerHarnessSummary,
  shouldShowApplyNewVersion,
  sortPresets,
  type OwnerFileLike,
  type PresetSummaryLike,
} from "#lib/screens/presets/presetsModel.js";

function summary(patch: Partial<PresetSummaryLike>): PresetSummaryLike {
  return {
    id: patch.id ?? "11111111-1111-4111-8111-111111111111",
    kind: patch.kind ?? "custom",
    title: patch.title ?? "Мой пресет",
    description: patch.description ?? "",
    revision: patch.revision ?? 1,
    text_bytes: patch.text_bytes ?? null,
    created: patch.created ?? { by: "owner", conversation_id: null, conversation_title: null, at: null },
    last_edit: patch.last_edit ?? null,
    used_by: patch.used_by ?? 0,
  };
}

describe("presets/model", () => {
  it("origin_labels_for_owner_and_agent_presets", () => {
    expect(formatOrigin(summary({ created: { by: "owner", conversation_id: null, conversation_title: null, at: null } }))).toBe(
      "Создан владельцем",
    );
    expect(
      formatOrigin(
        summary({
          created: { by: "agent", conversation_id: "22222222-2222-4222-8222-222222222222", conversation_title: "Разбор", at: null },
        }),
      ),
    ).toBe("Создан агентом · «Разбор»");
    expect(
      formatOrigin(
        summary({
          last_edit: { by: "agent", conversation_id: "22222222-2222-4222-8222-222222222222", conversation_title: "Разбор", at: "2026-10-29T12:00:00.000Z" },
        }),
      ),
    ).toContain("Изменён агентом");
  });

  it("byte_counter_and_limit", () => {
    expect(byteCounter(0).text).toBe("0 КиБ из 64 КиБ");
    expect(byteCounter(12_288).text).toBe("12,0 КиБ из 64 КиБ");
    expect(byteCounter(65_536).over).toBe(false);
    expect(byteCounter(65_537).over).toBe(true);
  });

  it("apply_new_version_shown_only_when_revision_is_behind", () => {
    expect(shouldShowApplyNewVersion({ current_revision: 2, revision: 1 })).toBe(true);
    expect(shouldShowApplyNewVersion({ current_revision: 2, revision: 2 })).toBe(false);
    expect(shouldShowApplyNewVersion({ current_revision: null, revision: 1 })).toBe(false);
  });

  it("builtins_sorted_first", () => {
    const items = [
      summary({ id: "b", kind: "custom", title: "Бета" }),
      summary({ id: "owner", kind: "owner", title: "Как у владельца" }),
      summary({ id: "a", kind: "custom", title: "Альфа" }),
      summary({ id: "clean", kind: "clean", title: "Без инструкций" }),
    ];
    expect(sortPresets(items).map((item) => item.id)).toEqual(["clean", "owner", "a", "b"]);
  });

  it("agent_mark_flags_presets_created_or_edited_by_agent", () => {
    expect(isAgentMarked(summary({}))).toBe(false);
    expect(
      isAgentMarked(summary({ created: { by: "agent", conversation_id: null, conversation_title: null, at: null } })),
    ).toBe(true);
    expect(
      isAgentMarked(summary({ last_edit: { by: "agent", conversation_id: null, conversation_title: null, at: null } })),
    ).toBe(true);
  });

  it("owner_harness_summary_counts_found_files_and_total_size", () => {
    const files: OwnerFileLike[] = [
      { kind: "claude_user", harness: "claude", path: "/a", present: true, size: 100, mtime: null, reason: "" },
      { kind: "claude_home", harness: "claude", path: "/b", present: false, size: 0, mtime: null, reason: "symlink" },
      { kind: "codex_global", harness: "codex", path: "/c", present: true, size: 2048, mtime: null, reason: "" },
      { kind: "opencode_global", harness: "opencode", path: "/d", present: false, size: 0, mtime: null, reason: "" },
    ];
    const summary = ownerHarnessSummary(files);
    expect(summary.find((item) => item.harness === "claude")).toMatchObject({ title: "Claude Code", found: 1, total: 100 });
    expect(summary.find((item) => item.harness === "codex")?.text).toContain("2,0 КиБ");
    expect(summary.find((item) => item.harness === "opencode")?.found).toBe(0);
  });

  it("owner_size_and_mtime_format", () => {
    expect(formatSize(0)).toBe("0 Б");
    expect(formatSize(2048)).toBe("2,0 КиБ");
    expect(formatOwnerMtime(null)).toBe("");
    const now = Date.parse("2026-11-03T12:00:00.000Z");
    expect(formatOwnerMtime("2026-10-28T12:00:00.000Z", now)).toMatch(/^28 октября, \d\d:00$/);
    expect(formatOwnerMtime("2025-10-28T12:00:00.000Z", now)).toMatch(/^28 октября 2025, \d\d:00$/);
    expect(memoryProjects(0)).toBe("нет проектов");
    expect(["1", "2", "5", "11", "21", "22"].map((n) => memoryProjects(Number(n)))).toEqual(["1 проект", "2 проекта", "5 проектов", "11 проектов", "21 проект", "22 проекта"]);
  });
});
