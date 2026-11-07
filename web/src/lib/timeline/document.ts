import { DOCUMENT_VIEW_MAX_BYTES } from './attachments.js';

/** Содержимое пришло больше разрешённого: разбирать его не будем, предложим скачать. */
export class DocumentTooLarge extends Error {
	constructor() {
		super('document_too_large');
		this.name = 'DocumentTooLarge';
	}
}

/** UTF-8 без метки порядка байтов: чужая метка попала бы в первый заголовок. */
export function decodeDocument(bytes: Uint8Array): string {
	const start = bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf ? 3 : 0;
	return new TextDecoder('utf-8').decode(bytes.subarray(start));
}

/**
 * Тот же адрес и те же куки, что у остальных вложений. Заголовок длины проверяем до чтения тела,
 * а саму длину — ещё раз после: сервер вправе не прислать заголовок.
 */
export async function fetchDocumentBytes(url: string, limit: number, signal?: AbortSignal): Promise<Uint8Array<ArrayBuffer>> {
	const response = await fetch(url, { credentials: 'same-origin', signal });
	if (!response.ok) throw new Error(`document_http_${response.status}`);

	const declared = Number(response.headers.get('content-length'));
	if (Number.isFinite(declared) && declared > limit) throw new DocumentTooLarge();

	const bytes = new Uint8Array(await response.arrayBuffer());
	if (bytes.byteLength > limit) throw new DocumentTooLarge();
	return bytes;
}

export async function fetchDocumentText(url: string, signal?: AbortSignal): Promise<string> {
	return decodeDocument(await fetchDocumentBytes(url, DOCUMENT_VIEW_MAX_BYTES, signal));
}
