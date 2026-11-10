import { beforeEach, describe, expect, it } from "vitest";
import { reasoningPref, saveReasoningPref } from "#lib/screens/settings/preferences.js";

beforeEach(() => localStorage.clear());

describe("reasoning preference", () => {
  it("is_live_by_default", () => {
    expect(reasoningPref()).toBe("live");
  });

  it("saves_and_reads_the_choice", () => {
    saveReasoningPref("line");
    expect(localStorage.getItem("canterlot.reasoning")).toBe("line");
    expect(reasoningPref()).toBe("line");
    saveReasoningPref("expanded");
    expect(reasoningPref()).toBe("expanded");
  });

  it("loads_a_saved_value_and_ignores_an_unknown_one", () => {
    localStorage.setItem("canterlot.reasoning", "expanded");
    expect(reasoningPref()).toBe("expanded");
    localStorage.setItem("canterlot.reasoning", "huge");
    expect(reasoningPref()).toBe("live");
  });
});
