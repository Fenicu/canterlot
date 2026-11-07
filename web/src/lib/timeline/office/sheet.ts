import type { SheetCell, SheetView } from './protocol.js';

/** Больше строк в таблицу не кладём: остальное доступно в скачанном файле. */
export const SHEET_ROW_CAP = 2000;
export const SHEET_COLUMN_CAP = 100;
/** Узкий столбец не уже этого, широкий не шире: длинный текст переносится внутри. */
export const SHEET_COLUMN_MIN_CH = 4;
export const SHEET_COLUMN_MAX_CH = 40;

const integerFmt = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 });
// Excel в формате «Общий» показывает около одиннадцати значащих цифр: шум двоичной дроби остаётся за кадром.
const fractionFmt = new Intl.NumberFormat('ru-RU', { maximumSignificantDigits: 11 });
const scientificFmt = new Intl.NumberFormat('ru-RU', { notation: 'scientific', maximumSignificantDigits: 6 });
const dateFmt = new Intl.DateTimeFormat('ru-RU', { timeZone: 'UTC', day: '2-digit', month: '2-digit', year: 'numeric' });
const timeFmt = new Intl.DateTimeFormat('ru-RU', { timeZone: 'UTC', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const secondsFmt = new Intl.DateTimeFormat('ru-RU', { timeZone: 'UTC', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });

const plain = (text: string) => text.replace(/[\u00a0\u202f]/g, ' ');

type Currency = 'RUB' | 'USD' | 'EUR';

/**
 * Числовой формат ячейки, разобранный из кода Excel. Даты сюда не попадают: библиотека уже отдала их
 * объектом `Date`, а код формата нужен только числам.
 */
interface ExcelNumberFormat {
  kind: 'percent' | 'currency' | 'number';
  minDigits: number;
  maxDigits: number;
  grouping: boolean;
  currency?: Currency;
  /** Текстовая валюта вроде «руб.»: символ ставится после числа как есть, без правил Intl. */
  suffix?: string;
}

const formatters = new Map<string, Intl.NumberFormat>();
function formatter(key: string, options: Intl.NumberFormatOptions): Intl.NumberFormat {
  let found = formatters.get(key);
  if (!found) {
    found = new Intl.NumberFormat('ru-RU', options);
    formatters.set(key, found);
  }
  return found;
}

/** Первая секция формата: всё до `;`, не считая точек с запятой внутри кавычек и скобок. */
function firstSection(code: string): string {
  let depth = 0;
  let quoted = false;
  for (let index = 0; index < code.length; index += 1) {
    const character = code[index]!;
    if (character === '"') quoted = !quoted;
    else if (!quoted && character === '[') depth += 1;
    else if (!quoted && character === ']') depth -= 1;
    else if (!quoted && depth === 0 && character === ';') return code.slice(0, index);
  }
  return code;
}

/**
 * Убирает служебное: цвета и условия в квадратных скобках (кроме валюты `[$₽-ru-RU]`),
 * кавычки и экранированные символы. Возвращает видимый шаблон и собранный текст валюты.
 */
function scanFormat(code: string): { visible: string; text: string; bracketCurrency?: string } {
  let visible = '';
  let text = '';
  let bracketCurrency: string | undefined;
  for (let index = 0; index < code.length; index += 1) {
    const character = code[index]!;
    if (character === '[') {
      const end = code.indexOf(']', index + 1);
      if (end < 0) break;
      const inner = code.slice(index + 1, end);
      if (inner.startsWith('$')) bracketCurrency = inner.slice(1).split('-')[0];
      index = end;
    } else if (character === '"') {
      const end = code.indexOf('"', index + 1);
      if (end < 0) break;
      text += code.slice(index + 1, end);
      index = end;
    } else if (character === '\\') {
      if (index + 1 < code.length) text += code[index + 1]!;
      index += 1;
    } else if (character === '_' || character === '*') {
      // Заполнитель ширины и повтор символа: сам символ в ячейке не показывается.
      index += 1;
    } else {
      visible += character;
    }
  }
  return { visible, text, bracketCurrency };
}

function currencyOf(symbol: string | undefined): Currency | undefined {
  if (!symbol) return undefined;
  if (symbol.includes('₽')) return 'RUB';
  if (symbol.includes('€')) return 'EUR';
  if (symbol.includes('$')) return 'USD';
  return undefined;
}

/** Разбирает код формата Excel; `null` — формат неизвестен, число показывается как раньше. */
export function parseExcelFormat(code: string): ExcelNumberFormat | null {
  const { visible, text, bracketCurrency } = scanFormat(firstSection(code));
  if (/[eE][+-]/.test(visible) || visible.includes('/')) return null;
  const numeric = visible.replace(/[^\d#?,.]/g, '');
  if (!/[0#?]/.test(numeric)) return null;
  const decimal = /\.([0#?]+)/.exec(numeric)?.[1] ?? '';
  const minDigits = decimal.replace(/[^0]/g, '').length;
  const maxDigits = decimal.length;
  const integer = numeric.split('.')[0] ?? '';
  const grouping = integer.includes(',');
  const rubles = /руб/i.test(text) || /руб/i.test(visible);
  const currency = currencyOf(bracketCurrency) ?? currencyOf(text) ?? currencyOf(visible);
  if (visible.includes('%')) return { kind: 'percent', minDigits, maxDigits, grouping };
  if (rubles) return { kind: 'number', minDigits, maxDigits, grouping, suffix: ' руб.' };
  if (currency) return { kind: 'currency', minDigits, maxDigits, grouping, currency };
  return { kind: 'number', minDigits, maxDigits, grouping };
}

function formatExcelNumber(value: number, code: string): string | null {
  const spec = parseExcelFormat(code);
  if (!spec) return null;
  if (spec.kind === 'percent') {
    return plain(formatter(`p:${spec.minDigits}:${spec.maxDigits}`, { style: 'percent', minimumFractionDigits: spec.minDigits, maximumFractionDigits: spec.maxDigits }).format(value));
  }
  if (spec.kind === 'currency' && spec.currency) {
    return plain(formatter(`c:${spec.currency}:${spec.minDigits}:${spec.maxDigits}`, { style: 'currency', currency: spec.currency, currencyDisplay: 'symbol', minimumFractionDigits: spec.minDigits, maximumFractionDigits: spec.maxDigits }).format(value));
  }
  const number = plain(formatter(`n:${spec.grouping ? 1 : 0}:${spec.minDigits}:${spec.maxDigits}`, { useGrouping: spec.grouping, minimumFractionDigits: spec.minDigits, maximumFractionDigits: spec.maxDigits }).format(value));
  return number + (spec.suffix ?? '');
}

function formatNumber(value: number): string {
  if (!Number.isFinite(value)) return String(value);
  const size = Math.abs(value);
  if (size !== 0 && (size >= 1e15 || size < 1e-9)) return plain(scientificFmt.format(value));
  return plain(Number.isInteger(value) ? integerFmt.format(value) : fractionFmt.format(value));
}

/**
 * Дата в Excel — дробное число дней без часового пояса; библиотека отдаёт её как UTC.
 * Дробь от деления даёт 18:29:59.999 вместо 18:30, поэтому сначала округляем до секунды.
 */
function formatDate(value: Date): string {
  const time = value.getTime();
  if (!Number.isFinite(time)) return '';
  const rounded = new Date(Math.round(time / 1000) * 1000);
  const day = dateFmt.format(rounded);
  if (rounded.getUTCHours() === 0 && rounded.getUTCMinutes() === 0 && rounded.getUTCSeconds() === 0) return day;
  return `${day}, ${(rounded.getUTCSeconds() ? secondsFmt : timeFmt).format(rounded)}`;
}

/** Сохранённое значение ячейки как текст для ru-RU. У формулы это её последний посчитанный результат. */
export function formatCell(value: unknown, format?: string): SheetCell {
  if (value === null || value === undefined) return { text: '', numeric: false };
  if (typeof value === 'number') {
    const formatted = format ? formatExcelNumber(value, format) : null;
    return { text: formatted ?? formatNumber(value), numeric: true };
  }
  if (typeof value === 'boolean') return { text: value ? 'ИСТИНА' : 'ЛОЖЬ', numeric: false };
  if (value instanceof Date) return { text: formatDate(value), numeric: true };
  return { text: String(value), numeric: false };
}

/** Пустые строки и столбцы в хвосте не показываем: Excel часто хранит отформатированную пустоту. */
function trimmed(data: readonly (readonly unknown[])[]): { rows: number; columns: number } {
  const empty = (value: unknown) => value === null || value === undefined || value === '';
  let rows = data.length;
  while (rows > 0 && data[rows - 1]!.every(empty)) rows -= 1;
  let columns = 0;
  for (let row = 0; row < rows; row += 1) {
    const cells = data[row]!;
    for (let column = cells.length - 1; column >= columns; column -= 1) {
      if (!empty(cells[column])) {
        columns = column + 1;
        break;
      }
    }
  }
  return { rows, columns };
}

/** Лист для показа: обрезанный по строкам и столбцам, с подсчётом того, что осталось за кадром. */
export function sheetView(name: string, data: readonly (readonly unknown[])[], formats?: readonly (readonly (string | undefined)[])[]): SheetView {
  const { rows: totalRows, columns: totalColumns } = trimmed(data);
  const shownColumns = Math.min(totalColumns, SHEET_COLUMN_CAP);
  const shown = Math.min(totalRows, SHEET_ROW_CAP);
  const rows: SheetCell[][] = [];
  const widths = Array.from({ length: shownColumns }, () => SHEET_COLUMN_MIN_CH);
  for (let row = 0; row < shown; row += 1) {
    const source = data[row]!;
    const cells: SheetCell[] = [];
    for (let column = 0; column < shownColumns; column += 1) {
      const cell = formatCell(source[column], formats?.[row]?.[column]);
      widths[column] = Math.max(widths[column]!, Math.min(SHEET_COLUMN_MAX_CH, cell.text.length));
      cells.push(cell);
    }
    rows.push(cells);
  }
  return { name, rows, totalRows, totalColumns, shownColumns, widths };
}
