import type { OfficeFailure } from './protocol.js';

/**
 * Распакованное содержимое крупнее этого не разбираем: десять мегабайт архива с XML
 * могут развернуться в гигабайты, и вкладка телефона упадёт по памяти.
 */
export const OFFICE_INFLATED_MAX_BYTES = 160 * 1024 * 1024;

const ZIP = [0x50, 0x4b, 0x03, 0x04];
const COMPOUND = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
const END_OF_DIRECTORY = 0x06054b50;
const DIRECTORY_ENTRY = 0x02014b50;

function startsWith(bytes: Uint8Array, signature: number[]): boolean {
  return signature.every((byte, index) => bytes[index] === byte);
}

/** Имя потока в каталоге составного файла записано в UTF-16LE. */
function containsWide(bytes: Uint8Array, text: string): boolean {
  const first = text.charCodeAt(0);
  outer: for (let index = 0; index + text.length * 2 <= bytes.length; index += 1) {
    if (bytes[index] !== first || bytes[index + 1] !== 0) continue;
    for (let char = 1; char < text.length; char += 1) {
      if (bytes[index + char * 2] !== text.charCodeAt(char) || bytes[index + char * 2 + 1] !== 0) continue outer;
    }
    return true;
  }
  return false;
}

/**
 * Сумма распакованных размеров по центральному каталогу ZIP.
 * Нет каталога — архив обрезан или это не ZIP: вернётся null.
 */
export function inflatedSize(bytes: Uint8Array): number | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  // Конец каталога — последние 22 байта плюс комментарий до 64 КиБ.
  const floor = Math.max(0, bytes.length - 22 - 0xffff);
  let end = -1;
  for (let index = bytes.length - 22; index >= floor; index -= 1) {
    if (view.getUint32(index, true) === END_OF_DIRECTORY) {
      end = index;
      break;
    }
  }
  if (end < 0) return null;
  const count = view.getUint16(end + 10, true);
  let offset = view.getUint32(end + 16, true);
  let total = 0;
  for (let entry = 0; entry < count; entry += 1) {
    if (offset + 46 > bytes.length || view.getUint32(offset, true) !== DIRECTORY_ENTRY) return null;
    const size = view.getUint32(offset + 24, true);
    // ZIP64 прячет настоящий размер в расширении: считаем такой файл заведомо огромным.
    total += size === 0xffffffff ? Number.POSITIVE_INFINITY : size;
    offset += 46 + view.getUint16(offset + 28, true) + view.getUint16(offset + 30, true) + view.getUint16(offset + 32, true);
  }
  return total;
}

/**
 * Что за файл пришёл, по первым байтам, а не по имени: .docx и .xlsx — это ZIP,
 * старые .doc и .xls — составной файл OLE2, и в такой же контейнер Office кладёт
 * файл под паролем, с потоком EncryptedPackage внутри.
 */
export function officeProblem(bytes: Uint8Array): OfficeFailure | null {
  if (startsWith(bytes, COMPOUND)) return containsWide(bytes, 'EncryptedPackage') ? 'locked' : 'legacy';
  if (!startsWith(bytes, ZIP)) return 'corrupt';
  const size = inflatedSize(bytes);
  if (size === null) return 'corrupt';
  if (size > OFFICE_INFLATED_MAX_BYTES) return 'too_large';
  return null;
}
