export interface Insertion {
  text: string;
  caret: number;
}

/** Вставка заготовки в поле ввода: пустое поле заменяется, иначе текст идёт в курсор. */
export function insertSnippet(
  draft: string,
  body: string,
  start: number,
  end: number,
): Insertion {
  const value = body.trim();
  if (!value) return { text: draft, caret: Math.min(Math.max(end, 0), draft.length) };
  if (!draft.trim()) return { text: body, caret: body.length };

  const from = Math.min(Math.max(start, 0), draft.length);
  const to = Math.min(Math.max(Math.max(end, from), 0), draft.length);
  const before = from > 0 && !/\s/.test(draft[from - 1]!) ? " " : "";
  const after = to < draft.length && !/\s/.test(draft[to]!) ? " " : "";
  const inserted = `${before}${value}${after}`;
  return {
    text: draft.slice(0, from) + inserted + draft.slice(to),
    caret: from + inserted.length
  };
}
