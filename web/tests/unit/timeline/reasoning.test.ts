import { describe, expect, it } from "vitest";
import {
  liveLineShown,
  reasoningDuration,
  reasoningPresentation,
  reasoningPreview,
  reasoningTail,
  type ReasoningChoice,
  type ReasoningPref,
  type ReasoningPresentation,
} from "#lib/timeline/reasoning.js";

describe("reasoning presentation", () => {
  it("lets_the_device_preference_choose_until_the_owner_taps", () => {
    const table: [ReasoningPref, boolean, ReasoningChoice, ReasoningPresentation][] = [
      // Настройка без выбора владельца.
      ["live", true, null, "live"],
      ["live", false, null, "collapsed"],
      ["line", true, null, "collapsed"],
      ["line", false, null, "collapsed"],
      ["expanded", true, null, "expanded"],
      ["expanded", false, null, "expanded"],
      // Тап сильнее настройки в обе стороны.
      ["live", true, "open", "expanded"],
      ["live", false, "open", "expanded"],
      ["line", true, "open", "expanded"],
      ["line", false, "open", "expanded"],
      ["expanded", true, "closed", "collapsed"],
      ["expanded", false, "closed", "collapsed"],
      ["live", true, "closed", "collapsed"],
      ["live", false, "closed", "collapsed"],
      ["line", false, "closed", "collapsed"],
    ];
    for (const [pref, streaming, choice, expected] of table) {
      expect(reasoningPresentation(pref, streaming, choice), `${pref}/${streaming}/${choice}`).toBe(expected);
    }
  });
});

describe("reasoning preview", () => {
  it("takes_the_first_meaningful_line_without_markdown", () => {
    expect(reasoningPreview("## План\n\n- **Проверить** `код`\n> дальше")).toBe("План");
    expect(reasoningPreview("- первый пункт\n- второй")).toBe("первый пункт");
    expect(reasoningPreview(">   цитата\n1. шаг")).toBe("цитата");
    expect(reasoningPreview("1. шаг\n2. шаг")).toBe("шаг");
    expect(reasoningPreview("   **Важно**  ")).toBe("Важно");
    expect(reasoningPreview("`одна мысль`")).toBe("одна мысль");
  });

  it("collapses_whitespace_and_cuts_long_text", () => {
    expect(reasoningPreview("  ##   Раз   \n два")).toBe("Раз");
    const long = reasoningPreview("а".repeat(400));
    expect(long).toHaveLength(120);
    expect(long).toBe("а".repeat(120));
  });

  it("returns_nothing_for_empty_text", () => {
    expect(reasoningPreview("")).toBe("");
    expect(reasoningPreview("\n\n   \n")).toBe("");
  });

  it("skips_fences_and_shows_the_code_inside", () => {
    expect(reasoningPreview("```\nconst x = 1\n```")).toBe("const x = 1");
    expect(reasoningPreview("```ts\nconst x = 1\n```")).toBe("const x = 1");
    expect(reasoningPreview("***")).toBe("");
  });

  it("shows_the_last_meaningful_line_in_the_live_flow", () => {
    expect(reasoningPreview("- первая\n- вторая", "live")).toBe("вторая");
    expect(reasoningPreview("**думаю**\n```\n`код`", "live")).toBe("код");
    expect(reasoningPreview("", "live")).toBe("");
  });
});

describe("reasoning tail", () => {
  it("keeps_short_text_whole", () => {
    expect(reasoningTail("первая\nвторая")).toBe("первая\nвторая");
  });

  it("cuts_from_the_start_of_a_line_and_keeps_the_end", () => {
    const text = Array.from({ length: 400 }, (_, index) => `строка ${index}`).join("\n");
    const tail = reasoningTail(text);
    expect(tail.length).toBeLessThanOrEqual(2000);
    expect(tail.startsWith("строка ")).toBe(true);
    expect(tail.endsWith("строка 399")).toBe(true);
    // Первая строка не обрезана посередине: хвост начинается с целой строки.
    expect(tail.split("\n")[0]).toMatch(/^строка \d+$/);
  });
});

describe("reasoning duration", () => {
  it("is_computed_from_the_block_marks", () => {
    expect(reasoningDuration({ startedAt: 1_000, createdAt: 900, finishedAt: 13_000 })).toBe("12 с");
    expect(reasoningDuration({ startedAt: null, createdAt: 1_000, finishedAt: 5_000 })).toBe("4 с");
  });

  it("is_empty_without_an_end_or_a_start", () => {
    expect(reasoningDuration({ startedAt: 1_000, createdAt: 1_000, finishedAt: null })).toBe("");
    expect(reasoningDuration({ startedAt: null, createdAt: 0, finishedAt: 5_000 })).toBe("");
  });
});

describe("turn live line", () => {
  it("does_not_repeat_the_word_about_thinking", () => {
    expect(liveLineShown("thinking", true)).toBe(false);
    // Пустое рассуждение не занимает строку: тогда о фазе говорит строка хода.
    expect(liveLineShown("thinking", false)).toBe(true);
    expect(liveLineShown("tool", true)).toBe(true);
    expect(liveLineShown("working", true)).toBe(true);
  });
});
