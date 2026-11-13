import { describe, expect, it } from "vitest";
import {
  FREQUENT_TERMS,
  addTerm,
  missingFrequent,
  termErrorText,
  validateTerms,
  type DictationTerm,
} from "#lib/dictation/terms.js";

describe("FREQUENT_TERMS", () => {
  it("holds_the_generic_pairs_in_order", () => {
    expect(FREQUENT_TERMS).toEqual([
      { pattern: "мейн", replacement: "main" },
      { pattern: "жесон", replacement: "JSON" },
      { pattern: "джейсон", replacement: "JSON" },
      { pattern: "апи", replacement: "API" },
      { pattern: "плейврайт", replacement: "Playwright" },
      { pattern: "плейрайт", replacement: "Playwright" },
      { pattern: "дипсик", replacement: "DeepSeek" },
      { pattern: "опус", replacement: "Opus" },
      { pattern: "некстест", replacement: "nextest" },
      { pattern: "гит", replacement: "git" },
      { pattern: "гитхаб", replacement: "GitHub" },
      { pattern: "докер", replacement: "Docker" },
      { pattern: "раст", replacement: "Rust" },
      { pattern: "свелт", replacement: "Svelte" },
    ]);
  });
});

describe("validateTerms", () => {
  const term = (pattern: string, replacement = "x"): DictationTerm => ({ pattern, replacement });

  it("accepts_a_short_unique_list", () => {
    expect(validateTerms([term("мейн", "main"), term("апи", "API")])).toBeNull();
  });

  it("rejects_more_than_two_hundred_pairs", () => {
    const list = Array.from({ length: 201 }, (_, index) => term(`слово${index}`));
    expect(validateTerms(list)).toBe("too_many_terms");
  });

  it("rejects_a_blank_pattern_or_replacement", () => {
    expect(validateTerms([{ pattern: "  ", replacement: "x" }])).toBe("term_empty");
    expect(validateTerms([{ pattern: "x", replacement: "" }])).toBe("term_empty");
  });

  it("rejects_a_term_longer_than_one_hundred_characters", () => {
    expect(validateTerms([term("а".repeat(101))])).toBe("term_too_long");
    expect(validateTerms([{ pattern: "а", replacement: "б".repeat(101) }])).toBe("term_too_long");
  });

  it("finds_duplicates_ignoring_case_yo_and_extra_spaces", () => {
    expect(validateTerms([term("Гит"), term(" гит ")])).toBe("duplicate_term");
    expect(validateTerms([term("ёлка", "ель"), term("елка", "ель")])).toBe("duplicate_term");
    expect(validateTerms([term("а  б"), term("а б")])).toBe("duplicate_term");
  });
});

describe("termErrorText", () => {
  it("speaks_russian_for_every_validation_code", () => {
    expect(termErrorText("too_many_terms")).toBe("Не больше 200 пар");
    expect(termErrorText("term_empty")).toBe("Заполните шаблон и замену");
    expect(termErrorText("term_too_long")).toBe("Шаблон и замена не длиннее 100 символов");
    expect(termErrorText("duplicate_term")).toBe("Такой шаблон уже есть");
    expect(termErrorText("unknown")).toBeNull();
  });
});

describe("missingFrequent", () => {
  it("offers_only_pairs_that_are_not_in_the_list_yet", () => {
    const list = [{ pattern: "Гит", replacement: "git" }];
    const options = missingFrequent(list);
    expect(options.find((item) => item.term.pattern === "гит")?.missing).toBe(false);
    expect(options.find((item) => item.term.pattern === "апи")?.missing).toBe(true);
    expect(options).toHaveLength(FREQUENT_TERMS.length);
  });
});

describe("addTerm", () => {
  it("adds_a_new_pair", () => {
    const result = addTerm([], { pattern: "апи", replacement: "API" });
    expect(result).toEqual({ ok: true, terms: [{ pattern: "апи", replacement: "API" }] });
  });

  it("refuses_a_duplicate_pattern", () => {
    const result = addTerm([{ pattern: "гит", replacement: "git" }], {
      pattern: " ГИТ ",
      replacement: "git",
    });
    expect(result).toEqual({ ok: false, error: "Такой шаблон уже есть" });
  });
});
