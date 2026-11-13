import { describe, expect, it } from "vitest";
import { insertDictated } from "#lib/dictation/insertion.js";

describe("insertDictated", () => {
  it("inserts_at_the_caret_with_spaces_around_a_word", () => {
    const draft = "Начало текста";
    const result = insertDictated(draft, { text: draft, start: 6, end: 6 }, "вставка");
    expect(result.text).toBe("Начало вставка текста");
    expect(result.text.slice(0, result.caret)).toBe("Начало вставка");
  });

  it("keeps_the_selection_and_inserts_after_it", () => {
    const draft = "начало конец хвост";
    const result = insertDictated(draft, { text: draft, start: 0, end: 7 }, "слово");
    expect(result.text).toBe("начало слово конец хвост");
  });

  it("adds_no_leading_space_at_the_start_of_the_draft", () => {
    expect(insertDictated("", { text: "", start: 0, end: 0 }, "привет").text).toBe("привет");
    expect(insertDictated("хвост", { text: "хвост", start: 0, end: 0 }, "привет").text).toBe(
      "привет хвост",
    );
  });

  it("adds_no_extra_spaces_next_to_whitespace", () => {
    const draft = "а б";
    expect(insertDictated(draft, { text: draft, start: 1, end: 1 }, "слово").text).toBe("а слово б");
    expect(insertDictated("слово\n\n", { text: "слово\n\n", start: 7, end: 7 }, "ещё").text).toBe(
      "слово\n\nещё",
    );
  });

  it("trims_surrounding_whitespace_of_the_dictated_text", () => {
    expect(insertDictated("а", { text: "а", start: 1, end: 1 }, "  б  ").text).toBe("а б");
  });

  it("does_not_change_the_draft_for_empty_dictation", () => {
    const result = insertDictated("черновик", { text: "черновик", start: 3, end: 3 }, "   ");
    expect(result).toEqual({ text: "черновик", caret: 3 });
  });

  it("inserts_at_the_anchor_when_only_the_tail_changed", () => {
    const result = insertDictated(
      "привет мир, ещё",
      { text: "привет мир", start: 6, end: 6 },
      "слово",
    );
    expect(result.text).toBe("привет слово мир, ещё");
  });

  it("appends_when_the_draft_changed_before_the_anchor", () => {
    const result = insertDictated(
      "новое привет мир",
      { text: "привет мир", start: 6, end: 6 },
      "слово",
    );
    expect(result.text).toBe("новое привет мир слово");
  });

  it("appends_when_the_anchor_moved_past_the_new_draft", () => {
    const result = insertDictated("да", { text: "давным-давно", start: 12, end: 12 }, "слово");
    expect(result.text).toBe("да слово");
  });

  it("puts_the_caret_after_the_inserted_text", () => {
    const result = insertDictated("а б", { text: "а б", start: 2, end: 2 }, "слово");
    expect(result.text).toBe("а слово б");
    expect(result.caret).toBe("а слово ".length);
  });
});
