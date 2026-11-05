import { describe, expect, it } from "vitest";
import { meterTone, windowText } from "#lib/stores/limits.js";
import { checkDraft, draftFrom, sameSettings } from "#lib/screens/settings/limitSettings.js";

const NOW = new Date("2026-11-05T12:00:00.000Z").getTime();

describe("limit settings", () => {
  it("meter_tone_uses_the_saved_thresholds", () => {
    const thresholds = { warn_remaining_pct: 40, danger_remaining_pct: 10 };
    expect(meterTone(60, thresholds)).toBe("normal");
    expect(meterTone(39, thresholds)).toBe("warning");
    expect(meterTone(10, thresholds)).toBe("warning");
    expect(meterTone(9, thresholds)).toBe("danger");
    expect(meterTone(0, thresholds)).toBe("danger");
    // Без второго аргумента действуют умолчания хаба.
    expect(meterTone(19)).toBe("warning");
    expect(meterTone(4)).toBe("danger");
  });

  it("window_tone_follows_the_config_thresholds", () => {
    const window = {
      used_permille: 700,
      resets_at: new Date("2026-11-06T12:00:00.000Z").toISOString(),
      observed_at: new Date("2026-11-05T11:59:00.000Z").toISOString(),
    };
    expect(windowText(window, NOW, {
      thresholds: { warn_remaining_pct: 40, danger_remaining_pct: 10 },
    }).tone).toBe("warning");
    expect(windowText(window, NOW, {
      thresholds: { warn_remaining_pct: 50, danger_remaining_pct: 35 },
    }).tone).toBe("danger");
    expect(windowText(window, NOW).tone).toBe("normal");
  });

  it("draft_round_trip_and_dirty_check", () => {
    const settings = { warn_remaining_pct: 25, danger_remaining_pct: 8, reserve_remaining_pct: 3 };
    const draft = draftFrom(settings);
    expect(draft).toEqual({ warn: 25, danger: 8, reserve: 3 });
    expect(sameSettings(draft, settings)).toBe(true);
    expect(sameSettings({ ...draft, reserve: 0 }, settings)).toBe(false);
  });

  it("validation_accepts_integers_from_0_to_99", () => {
    expect(checkDraft({ warn: 20, danger: 5, reserve: 0 })).toEqual({
      ok: true,
      settings: { warn_remaining_pct: 20, danger_remaining_pct: 5, reserve_remaining_pct: 0 },
    });
    expect(checkDraft({ warn: 99, danger: 99, reserve: 99 }).ok).toBe(true);
    expect(checkDraft({ warn: 0, danger: 0, reserve: 0 }).ok).toBe(true);
  });

  it("validation_rejects_empty_fractional_and_out_of_range_values", () => {
    for (const draft of [
      { warn: null, danger: 5, reserve: 5 },
      { warn: 20, danger: null, reserve: 5 },
      { warn: 20, danger: 5, reserve: null },
      { warn: 20.5, danger: 5, reserve: 5 },
      { warn: 100, danger: 5, reserve: 5 },
      { warn: 20, danger: 5, reserve: -1 },
    ]) {
      const checked = checkDraft(draft);
      expect(checked.ok).toBe(false);
      if (!checked.ok) expect(Object.keys(checked.errors).length).toBeGreaterThan(0);
    }
  });

  it("validation_requires_danger_not_above_warning", () => {
    const checked = checkDraft({ warn: 10, danger: 11, reserve: 5 });
    expect(checked).toEqual({ ok: false, errors: { relation: "Красный порог не больше жёлтого" } });
    expect(checkDraft({ warn: 10, danger: 10, reserve: 5 }).ok).toBe(true);
  });
});
