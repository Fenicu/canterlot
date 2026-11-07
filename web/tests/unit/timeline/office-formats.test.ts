import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { parseSheetCellsXml, parseStylesXml, parseWorkbookSheets, readSheetFormats } from '#lib/timeline/office/xlsx-styles.js';

const STYLES = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="2">
    <numFmt numFmtId="164" formatCode="#,##0.00&quot; ₽&quot;"/>
    <numFmt numFmtId="165" formatCode="0.0%"/>
  </numFmts>
  <cellXfs count="4">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
    <xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
    <xf numFmtId="165" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  </cellXfs>
</styleSheet>`;

const SHEET = `<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>Подпись</t></is></c><c r="B1"><v>1</v></c></row>
    <row r="2"><c r="A2"><v>1750</v></c><c r="B2" s="2"><v>0.336</v></c></row>
    <row r="3"><c r="C3" s="3"><v>0.5</v></c></row>
  </sheetData>
</worksheet>`;

const WORKBOOK = `<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Сводка" sheetId="1" r:id="rId1"/><sheet name="Форматы" sheetId="2" r:id="rId2"/></sheets>
</workbook>`;

const RELS = `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>`;

describe('timeline/office/xlsx-styles', () => {
  it('styles_map_style_indexes_to_built_in_and_custom_format_codes', () => {
    expect(parseStylesXml(STYLES)).toEqual([undefined, undefined, '#,##0.00" ₽"', '0.0%']);
  });

  it('cell_styles_are_read_by_column_and_row_and_left_sparse', () => {
    const formats = parseSheetCellsXml(SHEET, parseStylesXml(STYLES));
    expect(formats[1]![0]).toBeUndefined();
    expect(formats[1]![1]).toBe('#,##0.00" ₽"');
    expect(formats[2]![2]).toBe('0.0%');
    expect(formats[0]).toBeUndefined();
  });

  it('workbook_order_and_relationships_give_sheet_names_and_paths', () => {
    expect(parseWorkbookSheets(WORKBOOK, RELS)).toEqual([
      { name: 'Сводка', path: 'xl/worksheets/sheet1.xml' },
      { name: 'Форматы', path: 'xl/worksheets/sheet2.xml' }
    ]);
  });

  it('reads_formats_from_the_generated_fixture', () => {
    const bytes = readFileSync('testing/fixtures/office/table.xlsx');
    const archive = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
    const formats = readSheetFormats(archive);
    expect(formats.get('Форматы')?.[1]?.[1]).toBe('0%');
    expect(formats.get('Форматы')?.[2]?.[1]).toBe('0.0%');
    expect(formats.get('Форматы')?.[4]?.[1]).toBe('#,##0.00\\ "₽"');
    expect(formats.get('Журнал')?.[1]?.[0]).toBeUndefined();
  });
});
