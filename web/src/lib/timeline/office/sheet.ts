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

const plain = (text: string) => text.replace(/[  ]/g, ' ');

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
export function formatCell(value: unknown): SheetCell {
  if (value === null || value === undefined) return { text: '', numeric: false };
  if (typeof value === 'number') return { text: formatNumber(value), numeric: true };
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
export function sheetView(name: string, data: readonly (readonly unknown[])[]): SheetView {
  const { rows: totalRows, columns: totalColumns } = trimmed(data);
  const shownColumns = Math.min(totalColumns, SHEET_COLUMN_CAP);
  const shown = Math.min(totalRows, SHEET_ROW_CAP);
  const rows: SheetCell[][] = [];
  const widths = Array.from({ length: shownColumns }, () => SHEET_COLUMN_MIN_CH);
  for (let row = 0; row < shown; row += 1) {
    const source = data[row]!;
    const cells: SheetCell[] = [];
    for (let column = 0; column < shownColumns; column += 1) {
      const cell = formatCell(source[column]);
      widths[column] = Math.max(widths[column]!, Math.min(SHEET_COLUMN_MAX_CH, cell.text.length));
      cells.push(cell);
    }
    rows.push(cells);
  }
  return { name, rows, totalRows, totalColumns, shownColumns, widths };
}
