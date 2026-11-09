import { afterEach, describe, expect, it } from "vitest";
import {
  catalogModelLabel,
  changeSeen,
  markChangeSeen,
  resolutionChange,
  resolutionHint,
} from "#lib/format/modelResolution.js";

const NOW = Date.parse("2026-11-10T12:00:00.000Z");
const fable = {
  id: "fable[1m]",
  title: "Fable 1M",
  resolved: {
    model: "claude-fable-5-2",
    observed_at: "2026-11-10T09:30:00.000Z",
    previous: "claude-fable-5-1",
    changed_at: "2026-11-09T18:00:00.000Z",
  },
};

describe("format/model resolution", () => {
  afterEach(() => localStorage.clear());

  it("catalog_label_names_what_the_alias_runs_now", () => {
    expect(catalogModelLabel(fable)).toBe("Fable 1M — сейчас Fable 5.2");
    expect(catalogModelLabel({ id: "haiku", title: "Haiku" })).toBe("Haiku");
    expect(catalogModelLabel({ id: "gpt-6-luna", title: "gpt-6-luna", resolved: { model: "gpt-6-luna", observed_at: "2026-11-10T09:30:00.000Z" } })).toBe("gpt-6-luna");
  });

  it("hint_maps_alias_to_id_with_the_time_it_was_seen", () => {
    const hint = resolutionHint(fable, NOW);
    expect(hint.startsWith("fable[1m] → claude-fable-5-2 (по ходу ")).toBe(true);
    expect(resolutionHint({ id: "haiku", title: "Haiku" }, NOW)).toBe("");
  });

  it("change_is_news_for_a_week_and_then_quiet", () => {
    expect(resolutionChange(fable, NOW)?.text).toBe("fable[1m] теперь Fable 5.2, было Fable 5.1");
    expect(resolutionChange(fable, NOW + 8 * 24 * 3600 * 1000)).toBeNull();
    expect(resolutionChange({ ...fable, resolved: { ...fable.resolved, previous: null, changed_at: null } }, NOW)).toBeNull();
  });

  it("change_notice_is_remembered_as_seen_per_version", () => {
    const key = resolutionChange(fable, NOW)!.key;
    expect(changeSeen(key)).toBe(false);
    markChangeSeen(key);
    expect(changeSeen(key)).toBe(true);
    expect(changeSeen("fable[1m]:claude-fable-5-3")).toBe(false);
  });
});
