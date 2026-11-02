import { describe, expect, it } from "vitest";
import { limitsSubtitle } from "#lib/screens/limits/model.js";

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
});
