import { describe, expect, it } from "vitest";
import { limitStrip } from "#lib/screens/sessions/limitsText.js";
import {
  meterTone,
  remainingText,
  type ProviderLimits,
} from "#lib/stores/limits.js";

const NOW = new Date(2026, 9, 29, 12, 0).getTime();
const cfg = { stale_after_secs: 35 * 60, warn_pcts: [80, 95], weekly_reserve_pct: 95 };

function at(day: number, hour: number, minute: number): string {
  return new Date(2026, 9, day, hour, minute).toISOString();
}

function provider(
  name: string,
  fiveUsed: number | null,
  weekUsed: number | null,
  opts: { fiveAt?: string; weekAt?: string; observed?: string; reserve?: boolean } = {},
): ProviderLimits {
  const observed = opts.observed ?? new Date(NOW - 60_000).toISOString();
  return {
    provider: name,
    weekly_reserve_active: opts.reserve ?? false,
    five_hour:
      fiveUsed === null
        ? null
        : { used_permille: fiveUsed, resets_at: opts.fiveAt ?? at(29, 15, 30), observed_at: observed },
    weekly:
      weekUsed === null
        ? null
        : { used_permille: weekUsed, resets_at: opts.weekAt ?? at(31, 4, 0), observed_at: observed },
  };
}

describe("sessions/limits", () => {
  it("shows_the_remaining_share_not_the_used_one", () => {
    const strip = limitStrip(
      [provider("claude", 340, 960), provider("codex", 0, 150)],
      NOW,
      cfg,
    );
    const [claude, codex] = strip.rows;
    expect(claude?.five).toMatchObject({ text: "66%", fill: 66 });
    expect(claude?.week).toMatchObject({ text: "4%", fill: 4 });
    // Полный лимит показывается как 100%, полоска полная.
    expect(codex?.five).toMatchObject({ text: "100%", fill: 100 });
    expect(codex?.week).toMatchObject({ text: "85%", fill: 85 });
  });

  it("tone_follows_the_remaining_share_with_20_and_5_percent_edges", () => {
    expect(meterTone(100)).toBe("normal");
    expect(meterTone(20)).toBe("normal");
    expect(meterTone(19.9)).toBe("warning");
    expect(meterTone(5)).toBe("warning");
    expect(meterTone(4.9)).toBe("danger");
    expect(meterTone(0)).toBe("danger");
  });

  it("strip_tone_is_the_worst_fresh_meter_and_reserve_wins", () => {
    expect(limitStrip([provider("claude", 100, 100)], NOW, cfg).tone).toBe("default");
    expect(limitStrip([provider("claude", 100, 850)], NOW, cfg).tone).toBe("warning");
    expect(limitStrip([provider("claude", 100, 960)], NOW, cfg).tone).toBe("danger");
    const reserve = provider("claude", 100, 960, { reserve: true });
    const strip = limitStrip([provider("codex", 0, 0), reserve], NOW, cfg, { prefer: "claude" });
    expect(strip.tone).toBe("reserve");
    expect(strip.rows.map((row) => [row.label, row.reserve])).toEqual([
      ["Claude", true],
      ["Codex", false],
    ]);
  });

  it("remaining_text_rounds_and_never_claims_zero_while_something_is_left", () => {
    expect(remainingText(0)).toBe("100%");
    expect(remainingText(445)).toBe("56%");
    expect(remainingText(994)).toBe("<1%");
    expect(remainingText(1000)).toBe("0%");
  });

  it("a_window_whose_reset_has_passed_is_stale_even_when_the_reading_is_recent", () => {
    const strip = limitStrip(
      [provider("claude", 440, 530, { fiveAt: at(29, 11, 0) })],
      NOW,
      cfg,
    );
    const row = strip.rows[0]!;
    expect(row.five).toMatchObject({ stale: true, text: "устарело", reset: "" });
    expect(row.week).toMatchObject({ stale: false, text: "47%" });
    // Устаревшее окно не окрашивает полосу в тревожный цвет.
    expect(row.five.tone).toBe("normal");
  });

  it("readings_older_than_the_threshold_are_stale", () => {
    const old = new Date(NOW - 36 * 60_000).toISOString();
    const row = limitStrip([provider("claude", 100, 100, { observed: old })], NOW, cfg).rows[0]!;
    expect(row.five.stale).toBe(true);
    expect(row.week.stale).toBe(true);
    const recent = new Date(NOW - 34 * 60_000).toISOString();
    const fresh = limitStrip([provider("claude", 100, 100, { observed: recent })], NOW, cfg).rows[0]!;
    expect(fresh.five.stale).toBe(false);
  });

  it("reset_text_has_a_weekday_only_when_it_is_not_today", () => {
    const row = limitStrip(
      [provider("claude", 100, 100, { fiveAt: at(29, 19, 20), weekAt: at(31, 4, 0) })],
      NOW,
      cfg,
    ).rows[0]!;
    expect(row.five.reset).toBe("сброс 19:20");
    expect(row.week.reset).toBe("сброс сб 04:00");
  });

  it("updated_line_names_the_latest_reading_of_any_window", () => {
    const early = new Date(2026, 9, 29, 11, 40).toISOString();
    const late = new Date(2026, 9, 29, 11, 55).toISOString();
    const a = provider("claude", 100, 100, { observed: early });
    const b = provider("codex", 100, 100, { observed: late });
    expect(limitStrip([a, b], NOW, cfg).updated).toBe("обновлено 11:55");
    expect(limitStrip([], NOW, cfg).updated).toBe("");
  });

  it("absent_windows_say_no_data_instead_of_zero", () => {
    const empty = limitStrip(
      [{ provider: "claude", weekly_reserve_active: false, five_hour: null, weekly: null }],
      NOW,
      cfg,
    );
    expect(empty.rows[0]).toMatchObject({ label: "Claude", empty: true });
    expect(JSON.stringify(empty)).not.toContain("100%");
    const partial = limitStrip([provider("codex", 340, null)], NOW, cfg).rows[0]!;
    expect(partial).toMatchObject({ empty: false });
    expect(partial.five).toMatchObject({ text: "66%" });
    expect(partial.week).toMatchObject({ known: false, text: "нет данных", fill: 0 });
  });

  it("an_absent_five_hour_window_has_no_meter_and_no_stale_mark", () => {
    const claude: ProviderLimits = {
      provider: "claude",
      weekly_reserve_active: false,
      five_hour: null,
      five_hour_absent: true,
      weekly: { used_permille: 460, resets_at: at(31, 4, 0), observed_at: new Date(NOW - 60_000).toISOString() },
    };
    const strip = limitStrip([claude, provider("codex", 100, 100)], NOW, cfg);
    const row = strip.rows[0]!;
    expect(row).toMatchObject({ empty: false });
    expect(row.five.absent).toBe(true);
    expect(row.week).toMatchObject({ text: "54%", fill: 54, stale: false });
    expect(strip.rows[1]!.five.absent).toBe(false);
    // Отсутствующее окно не окрашивает полосу и не даёт «устарело».
    expect(strip.tone).toBe("default");
  });

  it("an_unknown_five_hour_window_without_the_absence_mark_still_says_no_data", () => {
    const row = limitStrip([provider("claude", null, 460)], NOW, cfg).rows[0]!;
    expect(row.five).toMatchObject({ absent: false, known: false, text: "нет данных" });
  });

  it("a_provider_with_only_an_absent_window_and_no_weekly_reading_stays_a_no_data_row", () => {
    const only: ProviderLimits = { provider: "claude", weekly_reserve_active: false, five_hour: null, five_hour_absent: true, weekly: null };
    expect(limitStrip([only], NOW, cfg).rows[0]).toMatchObject({ empty: true });
  });

  it("opencode_sorts_after_codex_and_keeps_its_label", () => {
    const strip = limitStrip(
      [provider("opencode", 125, 340), provider("codex", 0, 150), provider("claude", 340, 960)],
      NOW,
      cfg,
    );
    expect(strip.rows.map((row) => row.label)).toEqual(["Claude", "Codex", "OpenCode"]);
    expect(strip.rows[2]!).toMatchObject({ empty: false });
    expect(strip.rows[2]!.five).toMatchObject({ text: "88%" });
  });
});

