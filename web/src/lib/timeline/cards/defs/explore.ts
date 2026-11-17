// Чтение файла и поиск по коду: Read/Grep/Glob у Claude Code, read/grep/glob/list у OpenCode.
import { fmt } from '#lib/format/ru.js';
import { isObject, num, parseJson, str } from '../json';
import { toolId } from '../toolId';
import { clip } from '../copy';
import type { CardCall, CardLogic } from '../types';
import { fileName } from './files';

/** Подряд идущие чтения и поиски собираются в одну строку «Изучает код». */
export const EXPLORE_FOLD = 'explore';

export interface ReadData {
  path: string;
  /** Строки, если агент читал кусок: «стр. 120–180». */
  range: string;
}

/**
 * Окно чтения. У Claude Code `offset` — номер первой строки, у OpenCode — сколько строк
 * пропустить (с нуля). Без `limit` конец неизвестен: «с стр. 120».
 */
export function readRange(call: CardCall): string {
  const input = call.input;
  const offset = num(input.offset);
  const limit = num(input.limit);
  const zeroBased = call.name === 'read';
  const first = offset === null ? 1 : zeroBased ? offset + 1 : Math.max(1, offset);
  if (limit !== null && limit > 0) return `стр. ${first}–${first + limit - 1}`;
  if (offset !== null) return `с стр. ${first}`;
  const pages = str(input.pages);
  return pages ? `стр. PDF ${pages}` : '';
}

export const read: CardLogic<ReadData> = {
  quiet: true,
  fold: EXPLORE_FOLD,
  label: 'Чтение',
  parse(call) {
    // view_image Codex — картинка: её показывает блок инструмента с медиа, а не строка.
    if (call.name === 'view_image') return null;
    const path = (str(call.input.file_path) || str(call.input.filePath) || str(call.input.path) || (call.hasInput ? '' : call.argument)).trim();
    if (!path) return null;
    return { path, range: readRange(call) };
  },
  summary: (_call, data) => `Чтение ${fileName(data.path)}${data.range ? ` · ${data.range}` : ''}`
};

export type SearchKind = 'grep' | 'glob' | 'list';

export interface FindData {
  kind: SearchKind;
  pattern: string;
  /** Где искали: путь, маска файлов, тип. */
  where: string;
  /** Найдено: число и единица («файл», «совпадение»); `null`, пока ответа нет или он непонятен. */
  count: number | null;
  unit: 'files' | 'matches';
}

/**
 * Число находок из ответа. Claude Code и OpenCode пишут `Found N files` / `Found N matches`,
 * пустой ответ — `No files found` / `No matches found`; список путей Glob считается по строкам.
 */
export function foundCount(output: string, kind: SearchKind): { count: number | null; unit: 'files' | 'matches' } {
  const text = output.trim();
  const found = /^Found (\d+) (files?|matches?|lines?)/i.exec(text);
  if (found) return { count: Number(found[1]), unit: /^file/i.test(found[2]!) ? 'files' : 'matches' };
  if (/^No (?:files|matches) found/i.test(text)) return { count: 0, unit: kind === 'grep' ? 'matches' : 'files' };
  if (!text || kind === 'grep') return { count: null, unit: 'matches' };
  const lines = text.split('\n').filter((line) => line.trim() && !/^\(Results are truncated/i.test(line.trim()));
  return { count: lines.length, unit: 'files' };
}

function kindOf(call: CardCall): SearchKind {
  const name = call.name.toLowerCase();
  if (name === 'glob') return 'glob';
  if (name === 'list') return 'list';
  return 'grep';
}

export const find: CardLogic<FindData> = {
  quiet: true,
  fold: EXPLORE_FOLD,
  label: 'Поиск',
  parse(call) {
    const kind = kindOf(call);
    const input = call.input;
    const pattern = str(input.pattern).trim();
    const path = (str(input.path) || str(input.directory)).trim();
    const mask = (str(input.glob) || str(input.include) || (str(input.type) ? `*.${str(input.type)}` : '')).trim();
    if (!pattern && !(kind === 'list' && path)) return null;
    const where = [path, mask].filter(Boolean).join(' · ');
    return { kind, pattern, where, ...(call.state === 'done' ? foundCount(call.output, kind) : { count: null, unit: 'matches' as const }) };
  },
  summary(_call, data) {
    const head = data.kind === 'list' ? `Список ${data.where}` : `Поиск ${clip(data.pattern, 60)}${data.where ? ` в ${data.where}` : ''}`;
    return data.count === null ? head : `${head} · ${foundWord(data)}`;
  }
};

export function foundWord(data: Pick<FindData, 'count' | 'unit'>): string {
  if (data.count === null) return '';
  if (data.count === 0) return 'ничего';
  return `${data.count} ${fmt.plural(data.count, data.unit === 'files' ? ['файл', 'файла', 'файлов'] : ['совпадение', 'совпадения', 'совпадений'])}`;
}

/** Строка свёрнутой группы: «Изучает код: 6 файлов, 2 поиска». */
export function exploreLine(files: number, searches: number, opts: { running: boolean; voice: 'she' | 'neutral' }): string {
  const parts: string[] = [];
  if (files > 0) parts.push(`${files} ${fmt.plural(files, ['файл', 'файла', 'файлов'])}`);
  if (searches > 0) parts.push(`${searches} ${fmt.plural(searches, ['поиск', 'поиска', 'поисков'])}`);
  const head = opts.running ? 'Изучает код' : opts.voice === 'she' ? 'Изучила код' : 'Изучение кода';
  return `${head}: ${parts.join(', ')}`;
}

/** Сколько разных файлов прочитано и сколько было поисков: один файл, прочитанный кусками, — один файл. */
export function exploreCounts(blocks: { toolName: string | null; text: string }[]): { files: number; searches: number } {
  const files = new Set<string>();
  let reads = 0;
  let searches = 0;
  for (const block of blocks) {
    if (toolId(block.toolName) !== 'file.read') {
      searches += 1;
      continue;
    }
    reads += 1;
    const input = parseJson(block.text).value;
    const path = isObject(input) ? str(input.file_path) || str(input.filePath) || str(input.path) : '';
    if (path) files.add(path);
  }
  return { files: files.size || reads, searches };
}
