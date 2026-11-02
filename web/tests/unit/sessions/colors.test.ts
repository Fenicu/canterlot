import { describe, expect, it } from "vitest";
import { COLOR_KEYS, colorVar, toneVar } from "#lib/screens/sessions/colors.js";

describe("sessions/colors", () => {
  it("color_keys_map_to_tokens_and_unknown_falls_back_to_muted", () => {
    expect(COLOR_KEYS).toEqual([
      "accent",
      "blue",
      "amber",
      "success",
      "danger",
      "neutral",
    ]);
    expect(colorVar("accent")).toBe("--accent");
    expect(colorVar("blue")).toBe("--blue");
    expect(colorVar("amber")).toBe("--warning");
    expect(colorVar("success")).toBe("--success");
    expect(colorVar("danger")).toBe("--danger");
    expect(colorVar("neutral")).toBe("--muted");
    expect(colorVar(null)).toBe("--muted");
    expect(colorVar("purple")).toBe("--muted");
  });

  it("activity_tones_map_to_the_same_tokens_as_the_row_text", () => {
    expect(toneVar("accent")).toBe("--accent");
    expect(toneVar("warning")).toBe("--warning");
    expect(toneVar("danger")).toBe("--danger");
    expect(toneVar("default")).toBe("--muted");
  });
});
