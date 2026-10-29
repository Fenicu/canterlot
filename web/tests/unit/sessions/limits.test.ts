import { describe, expect, it } from "vitest";
import { limitStrip } from "#lib/screens/sessions/limitsText.js";
import type { ProviderLimits } from "#lib/stores/limits.js";

function at(hour: number, minute: number): string {
  return new Date(2026, 9, 30, hour, minute).toISOString();
}

function provider(
  name: string,
  five: number,
  week: number,
  fiveAt: string,
  weekAt: string,
  observed: string,
): ProviderLimits {
  return {
    provider: name,
    weekly_reserve_active: false,
    five_hour: {
      used_permille: five,
      resets_at: fiveAt,
      observed_at: observed,
    },
    weekly: { used_permille: week, resets_at: weekAt, observed_at: observed },
  };
}

describe("sessions/limits", () => {
  it("limit_strip_text_matches_the_mockup_and_marks_stale_data", () => {
    const now = new Date(2026, 9, 29, 12, 0).getTime();
    const fresh = new Date(now - 60_000).toISOString();
    const strip = limitStrip(
      [
        provider(
          "codex",
          90,
          150,
          at(2, 10),
          new Date(2026, 10, 1, 18, 0).toISOString(),
          fresh,
        ),
        provider(
          "claude",
          340,
          960,
          at(1, 30),
          new Date(2026, 9, 30, 9, 0).toISOString(),
          fresh,
        ),
      ],
      now,
      {
        stale_after_secs: 35 * 60,
        warn_pcts: [80, 95],
        weekly_reserve_pct: 95,
      },
    );
    expect(strip.summary).toBe(
      "Claude 5ч 34% · неделя 96%  Codex 5ч 9% · неделя 15%",
    );
    expect(strip.details).toContain(
      "Claude: 5ч до 01:30 · неделя до пт, 09:00",
    );
    expect(strip.tone).toBe("warning");
    const stale = limitStrip(
      [
        provider(
          "claude",
          100,
          100,
          at(1, 30),
          at(9, 0),
          new Date(now - 40 * 60_000).toISOString(),
        ),
      ],
      now,
      { stale_after_secs: 35 * 60, warn_pcts: [80], weekly_reserve_pct: 95 },
    );
    expect(stale.staleNote.startsWith("данные от ")).toBe(true);
  });

  it("limit_strip_thresholds_come_from_config", () => {
    const now = new Date(2026, 9, 29, 12, 0).getTime();
    const observed = new Date(now - 60_000).toISOString();
    const row = provider("claude", 700, 100, at(1, 30), at(9, 0), observed);
    expect(
      limitStrip([row], now, {
        stale_after_secs: 3600,
        warn_pcts: [80],
        weekly_reserve_pct: 95,
      }).tone,
    ).toBe("default");
    expect(
      limitStrip([row], now, {
        stale_after_secs: 3600,
        warn_pcts: [60],
        weekly_reserve_pct: 95,
      }).tone,
    ).toBe("warning");
    const reserve = { ...row, weekly_reserve_active: true };
    const crunch = limitStrip(
      [reserve],
      now,
      { stale_after_secs: 3600, warn_pcts: [80], weekly_reserve_pct: 95 },
      { crunch: true, prefer: "claude" },
    );
    expect(crunch.tone).toBe("reserve");
    expect(crunch.crunchLabel).toBe("Кранч · недельный резерв");
  });
});
