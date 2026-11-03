import { describe, expect, it } from "vitest";
import { limitsSubtitle, speedRows } from "#lib/screens/limits/model.js";

describe("screens/limits", () => {
  it("subtitle_names_every_provider_the_page_shows", () => {
    expect(limitsSubtitle([])).toBe("");
    expect(limitsSubtitle([{ provider: "claude" }])).toBe("Claude");
    expect(limitsSubtitle([{ provider: "claude" }, { provider: "codex" }])).toBe(
      "Claude и Codex",
    );
    expect(
      limitsSubtitle([
        { provider: "claude" },
        { provider: "codex" },
        { provider: "opencode" },
      ]),
    ).toBe("Claude, Codex и OpenCode");
  });

  it("subtitle_does_not_repeat_a_provider_twice", () => {
    expect(
      limitsSubtitle([{ provider: "codex" }, { provider: "codex" }]),
    ).toBe("Codex");
  });

  it("speed_rows_put_the_fastest_model_first_and_hide_models_without_numbers", () => {
    const empty = { runs: 0, tokens_per_second: null, first_token_ms: null };
    const rows = speedRows([
      { provider: "claude", model: "claude-opus-5-5", today: { runs: 2, tokens_per_second: 68.4, first_token_ms: 2100 }, period: { runs: 9, tokens_per_second: 71.2, first_token_ms: 1900 } },
      { provider: "opencode", model: "beam", today: empty, period: { runs: 3, tokens_per_second: 1240, first_token_ms: 420 } },
      { provider: "claude", model: "claude-haiku-4-5", today: empty, period: empty },
    ]);
    expect(rows.map((row) => row.model)).toEqual(["beam", "Opus 5.5"]);
    expect(rows[0]?.today).toEqual({ speed: null, firstToken: null });
    expect(rows[0]?.period).toEqual({ speed: "1 240 ток/с", firstToken: "первый токен 0,4 с" });
    expect(rows[1]?.provider).toBe("Claude");
  });
});
