import { flushSync } from "svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { reasoningPref, saveReasoningPref } from "#lib/screens/settings/preferences.js";
import { countEffectRuns } from "../stores/effect-runs.svelte";

beforeEach(() => localStorage.clear());

describe("reasoning preference", () => {
  it("is_live_by_default_and_reading_does_not_write", () => {
    expect(reasoningPref()).toBe("live");
    // Загрузка настроек не должна трогать хранилище, пока владелец ничего не выбрал.
    expect(localStorage.getItem("canterlot.reasoning")).toBeNull();
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

  it("saving_from_an_effect_does_not_retrigger_the_effect", () => {
    // Сохранение не читает ячейку, которую пишет: иначе эффект настроек зациклился бы на себе.
    const watch = countEffectRuns(() => saveReasoningPref("line"));
    flushSync();
    expect(watch.runs()).toBe(1);
    flushSync();
    expect(watch.runs()).toBe(1);
    watch.stop();
  });

  it("saving_wakes_an_effect_that_reads_the_preference", () => {
    let seen = "";
    const watch = countEffectRuns(() => {
      seen = reasoningPref();
    });
    flushSync();
    expect(seen).toBe("live");
    saveReasoningPref("expanded");
    flushSync();
    expect(seen).toBe("expanded");
    watch.stop();
  });
});
