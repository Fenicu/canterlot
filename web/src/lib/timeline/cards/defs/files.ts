// Правка и запись файла: Edit/MultiEdit/Write у Claude Code, edit/write у OpenCode, apply_patch и fileChange у Codex.
import { isObject, list, str, type JsonObject } from '../json';
import { applyPatchFiles, contentLines, countChanges, lineDiff, unifiedSections, type ChangeKind, type DiffLine, type FileChange } from '../diff';
import { bareArgument, callOutcome, type Outcome } from '../outcome';
import type { CardCall, CardLogic } from '../types';

export interface EditData {
  files: FileChange[];
  /** Вид перемены: у одного файла — его, у нескольких — общий, если он один на всех. */
  kind: ChangeKind;
  added: number;
  removed: number;
  /**
   * Строки одной общей разницей: Codex присылает куски нескольких файлов подряд без заголовков,
   * и разложить их по файлам нельзя. Пусто — у каждого файла свои строки.
   */
  shared: DiffLine[];
  /** Счёт строк известен: пока вызов идёт или Codex ничего не прислал, его нет. */
  counted: boolean;
  outcome: Outcome;
}

const VERB: Record<ChangeKind, string> = {
  add: 'Новый файл',
  edit: 'Правка',
  write: 'Запись',
  overwrite: 'Перезапись',
  delete: 'Удаление'
};

export function changeVerb(kind: ChangeKind): string {
  return VERB[kind];
}

export function fileName(path: string): string {
  const clean = path.replace(/\/+$/, '');
  return clean.slice(clean.lastIndexOf('/') + 1) || clean;
}

/** Папка файла коротко: последние две части пути, начало — многоточием. */
export function fileDir(path: string): string {
  const clean = path.replace(/\/+$/, '');
  const slash = clean.lastIndexOf('/');
  if (slash < 0) return '';
  const parts = clean.slice(0, slash).split('/');
  if (parts.length <= 3) return clean.slice(0, slash) || '/';
  return `…/${parts.slice(-2).join('/')}`;
}

function pathOf(input: JsonObject): string {
  return (str(input.file_path) || str(input.filePath) || str(input.path)).trim();
}

function oldNew(edit: JsonObject): { before: string; after: string } | null {
  const before = edit.old_string ?? edit.oldString;
  const after = edit.new_string ?? edit.newString;
  if (typeof before !== 'string' && typeof after !== 'string') return null;
  return { before: typeof before === 'string' ? before : '', after: typeof after === 'string' ? after : '' };
}

function file(path: string, kind: ChangeKind, lines: DiffLine[], content?: string): FileChange {
  return { path, kind, lines, ...countChanges(lines), ...(content !== undefined ? { content } : {}) };
}

function finish(call: CardCall, files: FileChange[], shared: DiffLine[] = [], counted = true): EditData | null {
  if (files.length === 0) return null;
  const kinds = new Set(files.map((item) => item.kind));
  const totals = shared.length > 0 ? countChanges(shared) : { added: files.reduce((sum, item) => sum + item.added, 0), removed: files.reduce((sum, item) => sum + item.removed, 0) };
  return { files, kind: kinds.size === 1 ? files[0]!.kind : 'edit', ...totals, shared, counted, outcome: callOutcome(call) };
}

/** Edit и MultiEdit у Claude Code, edit у OpenCode. */
export function editData(call: CardCall): EditData | null {
  const path = pathOf(call.input);
  if (!path) return null;
  const edits = list(call.input.edits).filter(isObject);
  if (edits.length > 0) {
    const lines = edits.flatMap((edit, index) => {
      const pair = oldNew(edit);
      if (!pair) return [];
      const diff = lineDiff(pair.before, pair.after);
      return edits.length > 1 ? [{ kind: 'hunk' as const, text: `правка ${index + 1} из ${edits.length}` }, ...diff] : diff;
    });
    return finish(call, [file(path, 'edit', lines)]);
  }
  const pair = oldNew(call.input);
  if (!pair) return null;
  return finish(call, [file(path, 'edit', lineDiff(pair.before, pair.after))]);
}

/** Write у Claude Code, write у OpenCode: новый текст файла целиком. */
export function writeData(call: CardCall): EditData | null {
  const path = pathOf(call.input);
  const content = call.input.content;
  if (!path || typeof content !== 'string') return null;
  const output = call.output;
  // Claude Code отвечает по-разному на создание и на перезапись; OpenCode — одинаково.
  const kind: ChangeKind = /^File created successfully/i.test(output) ? 'add' : /has been (?:updated|overwritten)/i.test(output) ? 'overwrite' : 'write';
  return finish(call, [file(path, kind, contentLines(content), content)]);
}

function patchText(call: CardCall): string {
  if (call.hasInput) return str(call.input.input) || str(call.input.patch) || str(call.input.patchText);
  return call.block.state === 'final' || call.block.state === 'incomplete' ? call.block.text : '';
}

/**
 * apply_patch Codex. Конверт `*** Begin Patch` разбирается по файлам. fileChange приходит
 * текстом разницы без путей: пути и итог лежат в сводке воркера (`a.ts, b.ts, completed`),
 * а у нового файла вместо разницы — его содержимое.
 */
export function patchData(call: CardCall): EditData | null {
  const text = patchText(call);
  const envelope = applyPatchFiles(text);
  if (envelope) return finish(call, envelope);
  const paths = bareArgument(call.argument).split(', ').map((path) => path.trim()).filter(Boolean);
  const sections = unifiedSections(text);
  if (sections.length > 0 && sections.every((section) => section.path)) {
    return finish(call, sections.map((section) => file(section.path, section.created ? 'add' : section.deleted ? 'delete' : 'edit', section.lines)));
  }
  if (paths.length === 0) return null;
  if (sections.length > 0) {
    const lines = sections.flatMap((section) => section.lines);
    if (paths.length === 1) return finish(call, [file(paths[0]!, 'edit', lines)]);
    return finish(call, paths.map((path) => file(path, 'edit', [])), lines);
  }
  if (text.trim() && paths.length === 1) return finish(call, [file(paths[0]!, 'add', contentLines(text), text.replace(/\n$/, ''))]);
  return finish(call, paths.map((path) => file(path, 'edit', [])), [], false);
}

function summary(_call: CardCall, data: EditData): string {
  const first = data.files[0]!;
  const more = data.files.length > 1 ? ` и ещё ${data.files.length - 1}` : '';
  const counts = data.counted ? ` · +${data.added} −${data.removed}` : '';
  return `${changeVerb(data.kind)} ${fileName(first.path)}${more}${counts}`;
}

export const edit: CardLogic<EditData> = { label: 'Правка', parse: editData, summary };
export const write: CardLogic<EditData> = { label: 'Запись', parse: writeData, summary };
export const patch: CardLogic<EditData> = { label: 'Правка', parse: patchData, summary };
