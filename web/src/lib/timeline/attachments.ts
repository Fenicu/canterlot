import type { AttachmentRef } from '#lib/stores/inputs.js';

export type AttachmentKind = 'image' | 'video' | 'file';

/** Крупнее картинку в пузыре не раскрывают сами: телефон декодирует её целиком. Открывается по касанию. */
export const THUMB_MAX_BYTES = 12 * 1024 * 1024;

const IMAGE_MIMES: ReadonlySet<string> = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp']);
const IMAGE_EXTENSIONS: ReadonlySet<string> = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp']);
const VIDEO_EXTENSIONS: ReadonlySet<string> = new Set(['mp4', 'mov', 'm4v', 'webm', 'mkv', '3gp']);

function extensionOf(name: string): string {
	const dot = name.lastIndexOf('.');
	return dot > 0 ? name.slice(dot + 1).toLowerCase() : '';
}

export function attachmentKind(file: Pick<AttachmentRef, 'name' | 'mime' | 'image'>): AttachmentKind {
	const generic = file.mime === '' || file.mime === 'application/octet-stream';
	if (file.image || IMAGE_MIMES.has(file.mime) || (generic && IMAGE_EXTENSIONS.has(extensionOf(file.name)))) return 'image';
	if (file.mime.startsWith('video/') || (generic && VIDEO_EXTENSIONS.has(extensionOf(file.name)))) return 'video';
	return 'file';
}

export function showsThumb(file: Pick<AttachmentRef, 'name' | 'mime' | 'image' | 'size'>): boolean {
	return attachmentKind(file) === 'image' && file.size <= THUMB_MAX_BYTES;
}

export function attachmentUrl(attachmentId: string): string {
	return `/api/v1/attachments/${encodeURIComponent(attachmentId)}/content`;
}
