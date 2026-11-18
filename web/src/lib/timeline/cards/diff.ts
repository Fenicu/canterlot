/**
 * Разница файла для карточки правки: строки из старого и нового текста (Edit, MultiEdit, edit
 * OpenCode), из unified diff (fileChange Codex) и из конверта `*** Begin Patch` (apply_patch).
 * Своя маленькая реализация: правки агента — короткие куски, на них хватает LCS по строкам.
 */
import { fmt } from '#lib/format/ru.js';

export type DiffKind = 'add' | 'del' | 'ctx' | 'hunk';

export interface DiffLine {
  kind: DiffKind;
  text: string;
}

export type ChangeKind = 'add' | 'edit' | 'write' | 'overwrite' | 'delete';

export interface FileChange {
  path: string;
  kind: ChangeKind;
  added: number;
  removed: number;
  lines: DiffLine[];
  /** Новый файл целиком: его показываем как код, без знаков разницы. */
  content?: string;
}

/** Строк контекста вокруг каждой перемены. */
export const DIFF_CONTEXT = 2;
/** Больше клеток таблица LCS не строит: дальше «всё старое убрано, всё новое добавлено». */
const LCS_CELLS = 4_000_000;

/** Строки текста без последнего перевода строки: `a\n` — одна строка, а не две. */
export function splitLines(text: string): string[] {
  if (!text) return [];
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines;
}

export function countChanges(lines: DiffLine[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const line of lines) {
    if (line.kind === 'add') added += 1;
    else if (line.kind === 'del') removed += 1;
  }
  return { added, removed };
}

function gap(count: number): DiffLine {
  return { kind: 'hunk', text: `${count} ${fmt.plural(count, ['строка', 'строки', 'строк'])} без изменений` };
}

function middle(a: string[], b: string[]): DiffLine[] {
  const n = a.length;
  const m = b.length;
  if (n === 0 || m === 0 || n * m > LCS_CELLS) {
    return [...a.map((text) => ({ kind: 'del' as const, text })), ...b.map((text) => ({ kind: 'add' as const, text }))];
  }
  const width = m + 1;
  const table = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i * width + j] = a[i] === b[j] ? table[(i + 1) * width + j + 1]! + 1 : Math.max(table[(i + 1) * width + j]!, table[i * width + j + 1]!);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ kind: 'ctx', text: a[i]! });
      i += 1;
      j += 1;
    } else if (table[(i + 1) * width + j]! >= table[i * width + j + 1]!) {
      out.push({ kind: 'del', text: a[i]! });
      i += 1;
    } else {
      out.push({ kind: 'add', text: b[j]! });
      j += 1;
    }
  }
  while (i < n) out.push({ kind: 'del', text: a[i++]! });
  while (j < m) out.push({ kind: 'add', text: b[j++]! });
  return out;
}

/** Неизменённые строки дальше `context` от перемен сворачиваются в строку «N строк без изменений». */
function compact(lines: DiffLine[], context: number): DiffLine[] {
  const changed = lines.map((line) => line.kind !== 'ctx');
  const keep = lines.map((_, index) => {
    for (let at = Math.max(0, index - context); at <= Math.min(lines.length - 1, index + context); at++) if (changed[at]) return true;
    return false;
  });
  const out: DiffLine[] = [];
  let skipped = 0;
  lines.forEach((line, index) => {
    if (keep[index]) {
      if (skipped > 0) out.push(gap(skipped));
      skipped = 0;
      out.push(line);
    } else skipped += 1;
  });
  if (skipped > 0) out.push(gap(skipped));
  return out;
}

/** Разница двух текстов по строкам с контекстом вокруг перемен. */
export function lineDiff(before: string, after: string, context = DIFF_CONTEXT): DiffLine[] {
  const a = splitLines(before);
  const b = splitLines(after);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head += 1;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail += 1;
  const lines: DiffLine[] = [
    ...a.slice(0, head).map((text) => ({ kind: 'ctx' as const, text })),
    ...middle(a.slice(head, a.length - tail), b.slice(head, b.length - tail)),
    ...a.slice(a.length - tail).map((text) => ({ kind: 'ctx' as const, text }))
  ];
  return compact(lines, context);
}

/** Новый файл: каждая строка добавлена. */
export function contentLines(text: string): DiffLine[] {
  return splitLines(text).map((line) => ({ kind: 'add', text: line }));
}

function hunkLine(line: string): DiffLine | null {
  if (line.startsWith('@@')) return { kind: 'hunk', text: line };
  if (line.startsWith('+')) return { kind: 'add', text: line.slice(1) };
  if (line.startsWith('-')) return { kind: 'del', text: line.slice(1) };
  if (line.startsWith(' ')) return { kind: 'ctx', text: line.slice(1) };
  if (line === '') return { kind: 'ctx', text: '' };
  return null;
}

const META = /^(?:diff --git |index |new file mode|deleted file mode|old mode|new mode|similarity index|rename from|rename to|Binary files|\\ No newline)/;

/** Строки unified diff без заголовков файла. */
export function unifiedLines(text: string): DiffLine[] {
  return unifiedSections(text).flatMap((section) => section.lines);
}

export interface DiffSection {
  /** Путь из заголовка `+++`; пусто, если заголовков нет (Codex присылает только куски). */
  path: string;
  created: boolean;
  deleted: boolean;
  lines: DiffLine[];
}

function headerPath(raw: string): string {
  const path = raw.replace(/\t.*$/, '').trim();
  return path.replace(/^[ab]\//, '');
}

/** Unified diff по файлам; текст без заголовков и кусков `@@` — содержимое файла. */
export function unifiedSections(text: string): DiffSection[] {
  const lines = splitLines(text);
  const hunk = lines.some((line) => line.startsWith('@@'));
  const header = lines.some((line, index) => line.startsWith('--- ') && lines[index + 1]?.startsWith('+++ '));
  if (!hunk && !header) return [];
  const sections: DiffSection[] = [];
  let current: DiffSection | null = null;
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]!;
    const next = lines[index + 1] ?? '';
    if (line.startsWith('--- ') && next.startsWith('+++ ')) {
      const from = headerPath(line.slice(4));
      const to = headerPath(next.slice(4));
      current = { path: to === '/dev/null' ? from : to, created: from === '/dev/null', deleted: to === '/dev/null', lines: [] };
      sections.push(current);
      index += 1;
      continue;
    }
    if (META.test(line)) continue;
    const parsed = hunkLine(line);
    if (!parsed) continue;
    if (!current) {
      current = { path: '', created: false, deleted: false, lines: [] };
      sections.push(current);
    }
    current.lines.push(parsed);
  }
  // Пустая строка после последнего куска — не контекст, а конец текста.
  for (const section of sections) while (section.lines.at(-1)?.kind === 'ctx' && section.lines.at(-1)!.text === '') section.lines.pop();
  return sections.filter((section) => section.lines.length > 0 || section.path);
}

/** Файлы из конверта apply_patch; `null` — это не конверт. */
export function applyPatchFiles(text: string): FileChange[] | null {
  const lines = splitLines(text.trim());
  if (lines[0]?.trim() !== '*** Begin Patch') return null;
  const files: FileChange[] = [];
  let current: FileChange | null = null;
  const added: string[] = [];
  const close = () => {
    if (!current) return;
    if (current.kind === 'add') current.content = added.join('\n');
    Object.assign(current, countChanges(current.lines));
    added.length = 0;
  };
  for (const line of lines.slice(1)) {
    const head = /^\*\*\* (Add|Update|Delete) File: (.+)$/.exec(line);
    if (head) {
      close();
      const kind: ChangeKind = head[1] === 'Add' ? 'add' : head[1] === 'Delete' ? 'delete' : 'edit';
      current = { path: head[2]!.trim(), kind, added: 0, removed: 0, lines: [] };
      files.push(current);
      continue;
    }
    const move = /^\*\*\* Move to: (.+)$/.exec(line);
    if (move && current) {
      current.path = move[1]!.trim();
      continue;
    }
    if (line.startsWith('*** ') || !current) continue;
    if (line.startsWith('@@')) {
      current.lines.push({ kind: 'hunk', text: line.slice(2).trim() });
      continue;
    }
    const parsed = hunkLine(line);
    if (!parsed || parsed.kind === 'hunk') continue;
    if (current.kind === 'add' && parsed.kind === 'add') added.push(parsed.text);
    current.lines.push(parsed);
  }
  close();
  return files;
}
