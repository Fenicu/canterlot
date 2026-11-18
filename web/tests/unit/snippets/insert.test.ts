import { describe, expect, it } from "vitest";
import { insertSnippet } from "#lib/snippets/insert.js";

describe("insertSnippet", () => {
  const body = "Проверь очередь задач";

  it("пустое поле заменяется целиком", () => {
    expect(insertSnippet("", body, 0, 0)).toEqual({ text: body, caret: body.length });
    expect(insertSnippet("   \n", body, 0, 0)).toEqual({ text: body, caret: body.length });
  });

  it("непустое поле получает текст в позиции курсора", () => {
    expect(insertSnippet("а б", body, 1, 1)).toEqual({
      text: `а ${body} б`,
      caret: 1 + body.length + 1
    });
  });

  it("вставка заменяет выбранный фрагмент", () => {
    expect(insertSnippet("а старый текст б", body, 2, 14)).toEqual({
      text: `а ${body} б`,
      caret: 1 + body.length + 1
    });
  });

  it("дальняя каретка не ломает вставку", () => {
    const result = insertSnippet("abc", body, 99, 99);
    expect(result.text).toBe(`abc ${body}`);
    expect(result.caret).toBe(result.text.length);
  });

  it("пустая заготовка ничего не меняет", () => {
    expect(insertSnippet("abc", "  ", 1, 1)).toEqual({ text: "abc", caret: 1 });
  });
});
