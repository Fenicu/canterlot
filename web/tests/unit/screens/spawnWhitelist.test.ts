import { describe, expect, it } from "vitest";
import {
  addEntry,
  describeEntries,
  harnessOptions,
  modelOptions,
  removeEntry,
  type CatalogHarness,
  type WhitelistView,
} from "#lib/screens/settings/spawnWhitelist.js";

const catalog: CatalogHarness[] = [
  {
    id: "claude_code",
    title: "Claude Code",
    models: [
      { id: "sonnet[1m]", title: "Sonnet 1M" },
      { id: "haiku", title: "Haiku" },
    ],
  },
  { id: "codex", title: "Codex", models: [{ id: "gpt-5", title: "GPT-5" }] },
];

const defaults = [
  { harness: "claude_code", model: "sonnet[1m]" },
  { harness: "claude_code", model: "haiku" },
  { harness: "codex", model: "default" },
];

describe("spawnWhitelist", () => {
  it("default_list_shows_as_default", () => {
    const view: WhitelistView = { entries: defaults, is_default: true, default_entries: defaults };
    const rows = describeEntries(view.entries, catalog);
    expect(rows.map((row) => row.modelTitle)).toEqual(["Sonnet 1M", "Haiku", "Модель по умолчанию"]);
    expect(rows.every((row) => row.known)).toBe(true);
  });

  it("duplicate_entry_is_refused", () => {
    const first = addEntry(defaults, { harness: "codex", model: "gpt-5" });
    expect(first.ok).toBe(true);
    const second = addEntry(defaults, { harness: "claude_code", model: "haiku" });
    expect(second).toEqual({ ok: false, error: "Такая запись уже есть" });
  });

  it("unknown_model_stays_visible", () => {
    const saved = [...defaults, { harness: "claude_code", model: "opus-old" }, { harness: "gone", model: "x" }];
    const rows = describeEntries(saved, catalog);
    expect(rows).toHaveLength(5);
    expect(rows[3]).toMatchObject({ model: "opus-old", modelTitle: "opus-old", known: false });
    expect(rows[4]).toMatchObject({ harnessTitle: "gone", known: false });
  });

  it("pickers_come_from_catalog_and_offer_default_model", () => {
    expect(harnessOptions(catalog)).toEqual([
      { value: "claude_code", label: "Claude Code" },
      { value: "codex", label: "Codex" },
    ]);
    expect(modelOptions(catalog, "codex")).toEqual([
      { value: "default", label: "Модель по умолчанию" },
      { value: "gpt-5", label: "GPT-5" },
    ]);
    expect(modelOptions(catalog, "missing")).toEqual([{ value: "default", label: "Модель по умолчанию" }]);
  });

  it("entry_limit_and_removal", () => {
    const many = Array.from({ length: 64 }, (_, n) => ({ harness: "codex", model: `m${n}` }));
    expect(addEntry(many, { harness: "codex", model: "extra" })).toEqual({ ok: false, error: "Не больше 64 записей" });
    expect(removeEntry(defaults, { harness: "codex", model: "default" })).toEqual(defaults.slice(0, 2));
  });
});
