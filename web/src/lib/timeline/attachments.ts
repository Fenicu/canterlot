import type { AttachmentRef } from '#lib/stores/inputs.js';

export type AttachmentKind = 'image' | 'video' | 'markdown' | 'word' | 'excel' | 'file';

/** Документ Word или таблица Excel: открываются в просмотрщике документов Office. */
export type OfficeKind = 'word' | 'excel';

/** Крупнее картинку в пузыре не раскрывают сами: телефон декодирует её целиком. Открывается по касанию. */
export const THUMB_MAX_BYTES = 12 * 1024 * 1024;

/** Документ крупнее этого не разбирают в просмотрщике: скачать можно, ждать разметку — нет. */
export const DOCUMENT_VIEW_MAX_BYTES = 2 * 1024 * 1024;

/** Файлы Word и Excel сжаты, поэтому им позволено больше: разбор идёт в фоновом потоке. */
export const OFFICE_VIEW_MAX_BYTES = 10 * 1024 * 1024;

const IMAGE_MIMES: ReadonlySet<string> = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp']);
const IMAGE_EXTENSIONS: ReadonlySet<string> = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp']);
const VIDEO_EXTENSIONS: ReadonlySet<string> = new Set(['mp4', 'mov', 'm4v', 'webm', 'mkv', '3gp']);
const MARKDOWN_MIMES: ReadonlySet<string> = new Set(['text/markdown', 'text/x-markdown']);
const MARKDOWN_EXTENSIONS: ReadonlySet<string> = new Set(['md', 'markdown', 'mdown', 'mkd']);
// Старые .doc и .xls тоже ведут в просмотрщик: он объяснит, что формат не поддерживается, и даст скачать.
const WORD_MIMES: ReadonlySet<string> = new Set([
	'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
	'application/msword'
]);
const WORD_EXTENSIONS: ReadonlySet<string> = new Set(['docx', 'doc']);
const EXCEL_MIMES: ReadonlySet<string> = new Set([
	'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
	'application/vnd.ms-excel'
]);
const EXCEL_EXTENSIONS: ReadonlySet<string> = new Set(['xlsx', 'xls']);
const OFFICE_LABELS: Record<OfficeKind, string> = { word: 'Документ Word', excel: 'Таблица Excel' };

function extensionOf(name: string): string {
	const dot = name.lastIndexOf('.');
	return dot > 0 ? name.slice(dot + 1).toLowerCase() : '';
}

export function attachmentKind(file: Pick<AttachmentRef, 'name' | 'mime' | 'image'> & { size?: number }): AttachmentKind {
	// Пустой файл с именем не бывает картинкой, видео или документом: только скачивание.
	if (file.size === 0 && file.name !== '') return 'file';
	const generic = file.mime === '' || file.mime === 'application/octet-stream';
	if (file.image || IMAGE_MIMES.has(file.mime) || (generic && IMAGE_EXTENSIONS.has(extensionOf(file.name)))) return 'image';
	if (file.mime.startsWith('video/') || (generic && VIDEO_EXTENSIONS.has(extensionOf(file.name)))) return 'video';
	if (MARKDOWN_MIMES.has(file.mime) || MARKDOWN_EXTENSIONS.has(extensionOf(file.name))) return 'markdown';
	if (WORD_MIMES.has(file.mime) || WORD_EXTENSIONS.has(extensionOf(file.name))) return 'word';
	if (EXCEL_MIMES.has(file.mime) || EXCEL_EXTENSIONS.has(extensionOf(file.name))) return 'excel';
	return 'file';
}

export function isOfficeKind(kind: AttachmentKind): kind is OfficeKind {
	return kind === 'word' || kind === 'excel';
}

/** Подпись вида в плитке и в шапке просмотрщика: «Документ Word», «Таблица Excel». */
export function officeLabel(kind: OfficeKind): string {
	return OFFICE_LABELS[kind];
}

/** Файл Word или Excel крупнее десяти мегабайт просмотрщик не загружает: только «Скачать». */
export function officeFitsViewer(size: number): boolean {
	return size <= OFFICE_VIEW_MAX_BYTES;
}

/** Просмотрщик берёт документ целиком: больше двух мегабайт он не разбирает. */
export function documentFitsViewer(size: number): boolean {
	return size <= DOCUMENT_VIEW_MAX_BYTES;
}

export function showsThumb(file: Pick<AttachmentRef, 'name' | 'mime' | 'image' | 'size'>): boolean {
	return attachmentKind(file) === 'image' && file.size <= THUMB_MAX_BYTES;
}

export function attachmentUrl(attachmentId: string): string {
	return `/api/v1/attachments/${encodeURIComponent(attachmentId)}/content`;
}
