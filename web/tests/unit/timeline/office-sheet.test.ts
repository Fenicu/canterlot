import { describe, expect, it } from 'vitest';
import { SHEET_COLUMN_CAP, SHEET_COLUMN_MAX_CH, SHEET_COLUMN_MIN_CH, SHEET_ROW_CAP, formatCell, sheetView } from '#lib/timeline/office/sheet.js';

describe('timeline/office/sheet', () => {
  it('numbers_use_russian_grouping_and_a_decimal_comma_without_binary_noise', () => {
    expect(formatCell(1_250_000.5)).toEqual({ text: '1 250 000,5', numeric: true });
    expect(formatCell(830_000).text).toBe('830 000');
    expect(formatCell(0.1 + 0.2).text).toBe('0,3');
    expect(formatCell(-42).text).toBe('-42');
    expect(formatCell(0).text).toBe('0');
    expect(formatCell(1.5e20).text).toMatch(/^1,5E20$/);
  });

  it('dates_are_day_month_year_and_show_time_only_when_there_is_one', () => {
    expect(formatCell(new Date('2026-10-01T00:00:00.000Z'))).toEqual({ text: '01.10.2026', numeric: true });
    // Сохранённая дробь дня даёт на миллисекунду меньше: показываем 18:30, а не 18:29.
    expect(formatCell(new Date('2026-10-31T18:29:59.999Z')).text).toBe('31.10.2026, 18:30');
    expect(formatCell(new Date('2026-10-31T07:05:09.000Z')).text).toBe('31.10.2026, 07:05:09');
    expect(formatCell(new Date(Number.NaN)).text).toBe('');
  });

  it('percent_formats_scale_by_hundred_and_keep_the_requested_decimals', () => {
    expect(formatCell(0.336, '0%')).toEqual({ text: '34 %', numeric: true });
    expect(formatCell(0.336, '0.0%').text).toBe('33,6 %');
    expect(formatCell(0.336, '0.00%').text).toBe('33,60 %');
  });

  it('currency_formats_use_russian_signs_grouping_and_fixed_decimals', () => {
    expect(formatCell(1750, '#,##0.00\\ "₽"').text).toBe('1 750,00 ₽');
    expect(formatCell(1750, '#,##0.00" €"').text).toBe('1 750,00 €');
    expect(formatCell(1750, '"$"#,##0.00').text).toBe('1 750,00 $');
    expect(formatCell(1750, '[$$-409]#,##0.00').text).toBe('1 750,00 $');
    expect(formatCell(1750, '#,##0.00" руб."').text).toBe('1 750,00 руб.');
  });

  it('thousands_and_fixed_decimals_follow_the_format_code', () => {
    expect(formatCell(1_234_567, '#,##0').text).toBe('1 234 567');
    expect(formatCell(1234.5, '0.00').text).toBe('1234,50');
    expect(formatCell(1234.5, '#,##0.000').text).toBe('1 234,500');
  });

  it('unknown_or_text_formats_fall_back_to_the_plain_number', () => {
    expect(formatCell(1234.5, 'General').text).toBe('1 234,5');
    expect(formatCell(1234.5, '0.0E+00').text).toBe('1 234,5');
    expect(formatCell(1234.5, '@').text).toBe('1 234,5');
  });

  it('sheet_view_passes_each_cell_format_to_the_formatter', () => {
    const view = sheetView('Форматы', [[0.336, 1750]], [['0.0%', '#,##0.00" ₽"']]);
    expect(view.rows[0]!.map((cell) => cell.text)).toEqual(['33,6 %', '1 750,00 ₽']);
  });

  it('text_booleans_and_empty_cells_keep_their_meaning', () => {
    expect(formatCell('Выручка')).toEqual({ text: 'Выручка', numeric: false });
    expect(formatCell(true).text).toBe('ИСТИНА');
    expect(formatCell(false).text).toBe('ЛОЖЬ');
    expect(formatCell(null)).toEqual({ text: '', numeric: false });
    expect(formatCell(undefined).text).toBe('');
  });

  it('a_short_sheet_is_shown_whole', () => {
    const view = sheetView('Сводка', Array.from({ length: 150 }, (_, row) => [row]));
    expect(view.rows).toHaveLength(150);
    expect(view.totalRows).toBe(150);
  });

  it('column_widths_follow_the_longest_text_within_bounds', () => {
    const view = sheetView('Лист', [['№', 'Комментарий', 'x'.repeat(200)], [1, 'коротко', '']]);
    expect(view.widths).toEqual([SHEET_COLUMN_MIN_CH, 'Комментарий'.length, SHEET_COLUMN_MAX_CH]);
  });

  it('a_long_sheet_keeps_the_first_rows_and_reports_the_full_count', () => {
    const data = Array.from({ length: 2501 }, (_, row) => [row === 0 ? 'Номер' : row, row === 0 ? 'Задача' : `Задача ${row}`]);
    const view = sheetView('Журнал', data);
    expect(view.name).toBe('Журнал');
    expect(SHEET_ROW_CAP).toBe(2000);
    expect(view.rows).toHaveLength(SHEET_ROW_CAP);
    expect(view.totalRows).toBe(2501);
    expect(view.rows[0]!.map((cell) => cell.text)).toEqual(['Номер', 'Задача']);
    expect(view.rows[1999]![1]!.text).toBe('Задача 1999');
  });

  it('a_wide_sheet_shows_the_first_hundred_columns', () => {
    const data = [Array.from({ length: 300 }, (_, column) => column)];
    const view = sheetView('Широкий', data);
    expect(view.totalColumns).toBe(300);
    expect(view.shownColumns).toBe(SHEET_COLUMN_CAP);
    expect(view.rows[0]).toHaveLength(SHEET_COLUMN_CAP);
  });

  it('trailing_empty_rows_and_columns_are_not_shown', () => {
    const view = sheetView('Лист', [
      ['a', 1, null, null],
      [null, null, null, ''],
      ['b', null, null, null],
      [null, null, null, null],
      ['', null, null, null]
    ]);
    expect(view.totalRows).toBe(3);
    expect(view.totalColumns).toBe(2);
    expect(view.rows.map((row) => row.map((cell) => cell.text))).toEqual([['a', '1'], ['', ''], ['b', '']]);
  });
});
