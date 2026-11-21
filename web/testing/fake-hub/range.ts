/**
 * Диапазоны выдачи файлов в фальшивом хабе: те же правила, что у настоящего хаба
 * (`crates/canterlot-hub/src/http/range.rs`). Один диапазон `a-b`, `a-` или `-n`;
 * несколько диапазонов, чужая единица и неверная запись дают весь файл.
 */
export type RangeAnswer =
  | { kind: 'full' }
  | { kind: 'partial'; start: number; end: number }
  | { kind: 'unsatisfiable' };

export function answerRange(header: string | undefined, size: number): RangeAnswer {
  if (!header) return { kind: 'full' };
  const match = /^\s*bytes\s*=\s*(\d*)\s*-\s*(\d*)\s*$/i.exec(header);
  if (!match) return { kind: 'full' };
  const [, first = '', last = ''] = match;
  if (first === '' && last === '') return { kind: 'full' };
  if (first === '') {
    const length = Number(last);
    if (length === 0 || size === 0) return { kind: 'unsatisfiable' };
    return { kind: 'partial', start: Math.max(0, size - length), end: size - 1 };
  }
  const start = Number(first);
  if (last !== '' && Number(last) < start) return { kind: 'full' };
  if (start >= size) return { kind: 'unsatisfiable' };
  return { kind: 'partial', start, end: last === '' ? size - 1 : Math.min(Number(last), size - 1) };
}

/** Что фальшивый хаб ответил на запрос содержимого: тест проверяет, что плеер ходит за диапазонами. */
export interface BlobRequest {
  id: string;
  range: string | null;
  status: number;
}
