import readExcelFile from 'read-excel-file/web-worker';
import type { OfficeReply, OfficeRequest } from './protocol.js';
import { sheetView } from './sheet.js';
import { readSheetFormats, type CellFormats } from './xlsx-styles.js';

const reply = (message: OfficeReply) => self.postMessage(message);

self.onmessage = async (event: MessageEvent<OfficeRequest>) => {
  try {
    // Числовые форматы библиотека не отдаёт: читаем `xl/styles.xml` и атрибут `s` ячеек из тех же байтов.
    let formats = new Map<string, CellFormats>();
    try {
      formats = readSheetFormats(event.data.bytes);
    } catch {
      // Повреждённый styles.xml не должен прятать сами значения: они покажутся как раньше.
    }
    const sheets = await readExcelFile(event.data.bytes);
    // Обрезка и форматирование здесь же: в главный поток уходят готовые строки, а не весь лист.
    reply({ kind: 'xlsx', sheets: sheets.map((sheet) => sheetView(sheet.sheet, sheet.data, formats.get(sheet.sheet))) });
  } catch (error) {
    const code = (error as { code?: string } | null)?.code;
    reply({ kind: 'failed', reason: code === 'XLS_FILE_NOT_SUPPORTED' ? 'legacy' : 'corrupt' });
  }
};
