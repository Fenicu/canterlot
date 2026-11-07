import readExcelFile from 'read-excel-file/web-worker';
import type { OfficeReply, OfficeRequest } from './protocol.js';
import { sheetView } from './sheet.js';

const reply = (message: OfficeReply) => self.postMessage(message);

self.onmessage = async (event: MessageEvent<OfficeRequest>) => {
  try {
    const sheets = await readExcelFile(event.data.bytes);
    // Обрезка и форматирование здесь же: в главный поток уходят готовые строки, а не весь лист.
    reply({ kind: 'xlsx', sheets: sheets.map((sheet) => sheetView(sheet.sheet, sheet.data)) });
  } catch (error) {
    const code = (error as { code?: string } | null)?.code;
    reply({ kind: 'failed', reason: code === 'XLS_FILE_NOT_SUPPORTED' ? 'legacy' : 'corrupt' });
  }
};
