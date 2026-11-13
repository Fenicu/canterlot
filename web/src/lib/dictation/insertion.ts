/**
 * Вставка распознанной речи в черновик (R12). Якорь — снимок черновика и выделения
 * на момент нажатия микрофона: удалённые правки за время записи не теряются.
 */
export interface DraftAnchor {
  text: string;
  start: number;
  end: number;
}

export interface InsertionResult {
  text: string;
  caret: number;
}

export function insertDictated(draft: string, anchor: DraftAnchor, dictated: string): InsertionResult {
  const core = dictated.trim();
  if (!core) return { text: draft, caret: Math.min(Math.max(anchor.end, 0), draft.length) };

  let at: number;
  if (draft === anchor.text) {
    at = anchor.end;
  } else if (draft.slice(0, anchor.end) === anchor.text.slice(0, anchor.end)) {
    at = anchor.end;
  } else {
    // Черновик изменился до якоря: место больше не найти, текст идёт в конец.
    at = draft.length;
  }
  at = Math.min(Math.max(at, 0), draft.length);

  const before = at > 0 && !/\s/.test(draft[at - 1]!) ? " " : "";
  const after = at < draft.length && !/\s/.test(draft[at]!) ? " " : "";
  const insert = `${before}${core}${after}`;
  return { text: draft.slice(0, at) + insert + draft.slice(at), caret: at + insert.length };
}
