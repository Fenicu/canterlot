/** Сообщения между просмотрщиком и фоновыми разборщиками файлов Word и Excel. */

/** Почему файл не показан: старый формат, пароль, повреждение или слишком тяжёлое содержимое. */
export type OfficeFailure = 'legacy' | 'locked' | 'corrupt' | 'too_large';

export interface SheetCell {
  text: string;
  /** Числа и даты выравниваются по правому краю, как в Excel. */
  numeric: boolean;
}

export interface SheetView {
  name: string;
  rows: SheetCell[][];
  totalRows: number;
  totalColumns: number;
  shownColumns: number;
  /** Ширина столбца в знаках: таблица раскладывается по ним сразу, не перебирая две тысячи строк. */
  widths: number[];
}

export interface DocxImage {
  type: string;
  data: ArrayBuffer;
}

export type OfficeRequest = { bytes: ArrayBuffer };

export type OfficeReply =
  | { kind: 'docx'; html: string; images: DocxImage[] }
  | { kind: 'xlsx'; sheets: SheetView[] }
  | { kind: 'failed'; reason: OfficeFailure };
