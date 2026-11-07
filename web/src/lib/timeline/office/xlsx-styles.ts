/** Чтение кодов числовых форматов из `xl/styles.xml`: библиотека отдаёт только значения ячеек. */

import { unzipSync } from 'fflate';

export type CellFormats = (string | undefined)[][];

export interface SheetFormats {
  name: string;
  path: string;
}

/** Встроенные форматы Excel, которые стоит показать. Даты остаются `Date` и в эту таблицу не входят. */
const BUILTIN_FORMATS: Record<number, string> = {
  1: '0',
  2: '0.00',
  3: '#,##0',
  4: '#,##0.00',
  5: '"$"#,##0_);("$"#,##0)',
  6: '"$"#,##0_);[Red]("$"#,##0)',
  7: '"$"#,##0.00_);("$"#,##0.00)',
  8: '"$"#,##0.00_);[Red]("$"#,##0.00)',
  9: '0%',
  10: '0.00%',
  11: '0.00E+00',
  12: '# ?/?',
  13: '# ??/??',
  37: '#,##0 ;(#,##0)',
  38: '#,##0 ;[Red](#,##0)',
  39: '#,##0.00;(#,##0.00)',
  40: '#,##0.00;[Red](#,##0.00)',
  41: '_(* #,##0_);_(* (#,##0);_(* "-"_);_(@_)',
  42: '_("$"* #,##0_);_("$"* (#,##0);_("$"* "-"_);_(@_)',
  43: '_(* #,##0.00_);_(* (#,##0.00);_(* "-"??_);_(@_)',
  44: '_("$"* #,##0.00_);_("$"* (#,##0.00);_("$"* "-"??_);_(@_)',
  48: '##0.0E+0',
  49: '@'
};

function decodeXml(value: string): string {
  return value.replace(/&(?:#(\d+)|#x([\da-f]+)|(amp|lt|gt|quot|apos));/gi, (_, decimal: string | undefined, hex: string | undefined, name: string | undefined) => {
    if (decimal) return String.fromCodePoint(Number(decimal));
    if (hex) return String.fromCodePoint(Number.parseInt(hex, 16));
    return { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" }[name ?? ''] ?? '';
  });
}

function attributes(tag: string): Record<string, string> {
  const found: Record<string, string> = {};
  for (const match of tag.matchAll(/([\w:.-]+)\s*=\s*"([^"]*)"/g)) found[match[1]!] = decodeXml(match[2]!);
  return found;
}

function columnIndex(letters: string): number {
  let index = 0;
  for (const letter of letters.toUpperCase()) index = index * 26 + letter.charCodeAt(0) - 64;
  return index - 1;
}

/** Индекс `cellXfs` — идентификатор стиля ячейки; значение — код формата или `undefined` для «Общего». */
export function parseStylesXml(xml: string): (string | undefined)[] {
  const custom = new Map<number, string>();
  for (const match of xml.matchAll(/<numFmt\b[^>]*>/g)) {
    const attributes_ = attributes(match[0]);
    if (attributes_.numFmtId !== undefined && attributes_.formatCode !== undefined) custom.set(Number(attributes_.numFmtId), attributes_.formatCode);
  }
  const block = /<cellXfs\b[^>]*>([\s\S]*?)<\/cellXfs>/.exec(xml);
  if (!block) return [];
  const formats: (string | undefined)[] = [];
  for (const match of block[1]!.matchAll(/<xf\b[^>]*>/g)) {
    const numFmtId = Number(attributes(match[0]).numFmtId ?? 0);
    formats.push(custom.get(numFmtId) ?? BUILTIN_FORMATS[numFmtId]);
  }
  return formats;
}

/** `s` у ячейки — индекс в `cellXfs`; разрешаем его в код формата и раскладываем по строкам и столбцам. */
export function parseSheetCellsXml(xml: string, styles: readonly (string | undefined)[]): CellFormats {
  const formats: CellFormats = [];
  for (const match of xml.matchAll(/<c\b([^>]*)>/g)) {
    const attributes_ = attributes(match[1]!);
    if (attributes_.s === undefined || attributes_.r === undefined) continue;
    const reference = /^([A-Z]+)(\d+)$/i.exec(attributes_.r);
    if (!reference) continue;
    const row = Number(reference[2]) - 1;
    const column = columnIndex(reference[1]!);
    if (row < 0 || column < 0) continue;
    const format = styles[Number(attributes_.s)];
    if (format !== undefined) (formats[row] ??= [])[column] = format;
  }
  return formats;
}

/** Порядок листов и путь к их XML: в книге листы идут не по номерам файлов. */
export function parseWorkbookSheets(workbook: string, rels: string): SheetFormats[] {
  const targets = new Map<string, string>();
  for (const match of rels.matchAll(/<Relationship\b[^>]*>/g)) {
    const attributes_ = attributes(match[0]);
    if (attributes_.Id && attributes_.Target) targets.set(attributes_.Id, attributes_.Target);
  }
  const sheets: SheetFormats[] = [];
  for (const match of workbook.matchAll(/<sheet\b[^>]*>/g)) {
    const attributes_ = attributes(match[0]);
    const relation = Object.entries(attributes_).find(([name]) => name === 'id' || name.endsWith(':id'))?.[1];
    const target = relation ? targets.get(relation) : undefined;
    if (!attributes_.name || !target) continue;
    const path = target.startsWith('/') ? target.slice(1) : target.startsWith('xl/') ? target : `xl/${target.replace(/^\.\//, '')}`;
    sheets.push({ name: attributes_.name, path });
  }
  return sheets;
}

/** Карта «имя листа → формат каждой ячейки». Ошибка чтения архива не должна ломать просмотр. */
export function readSheetFormats(bytes: ArrayBuffer): Map<string, CellFormats> {
  const files = unzipSync(new Uint8Array(bytes), {
    filter: (file) => /^xl\/(?:styles\.xml|workbook\.xml|_rels\/workbook\.xml\.rels|worksheets\/[^/]+\.xml)$/.test(file.name)
  });
  const decoder = new TextDecoder();
  const text = (name: string) => (files[name] ? decoder.decode(files[name]) : undefined);
  const stylesXml = text('xl/styles.xml');
  const workbook = text('xl/workbook.xml');
  const rels = text('xl/_rels/workbook.xml.rels');
  if (!stylesXml || !workbook || !rels) return new Map();
  const styles = parseStylesXml(stylesXml);
  const result = new Map<string, CellFormats>();
  for (const sheet of parseWorkbookSheets(workbook, rels)) {
    const xml = text(sheet.path);
    if (xml) result.set(sheet.name, parseSheetCellsXml(xml, styles));
  }
  return result;
}
