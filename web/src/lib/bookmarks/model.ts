import type { components } from '#lib/api/generated/openapi.js';
import type { BookmarkChanged } from '#lib/api/generated/events.js';
import { fmt } from '#lib/format/ru.js';
import { harnessLabel } from '#lib/format/labels.js';
import { hitHref } from '#lib/search/model.js';
import { agentTitle } from '#lib/timeline/present.js';

export type BookmarkView = components['schemas']['BookmarkView'];
export type BookmarkPage = components['schemas']['BookmarkPage'];
export type BookmarkTargetKind = BookmarkView['target_kind'];
export type BookmarkChange = BookmarkChanged['payload'];

/** В событии места ленты приходят строками; в HTTP — числами, как их ждёт лента. */
export type BookmarkEventView = Omit<BookmarkView, 'seq' | 'jump_seq'> & {
	seq: string;
	jump_seq: string;
};

export function bookmarkKey(
	kind: BookmarkTargetKind,
	targetId: string,
): string {
	return `${kind}:${targetId}`;
}

export function bookmarkTarget(entry: Pick<BookmarkView, 'target_kind' | 'target_id'>): string {
	return bookmarkKey(entry.target_kind, entry.target_id);
}

/** Провайдер в записи закладки — только идентификатор инструмента, как у совпадения поиска. */
function harnessProvider(id: string): string {
	return id.split(/[_\-\s]/)[0] ?? id;
}

/**
 * Одна закладка на карточку ответа: последний верхний текстовый блок, а без текста —
 * первый верхний блок. Вложенные блоки закладок не несут.
 */
export function bookmarkAnchor<T extends { id: string; kind: string; parentId: string | null }>(
	blocks: readonly T[],
): T | null {
	const top = blocks.filter((block) => block.parentId === null);
	if (top.length === 0) return null;
	return top.findLast((block) => block.kind === 'text') ?? top[0]!;
}

/** Карточка отмечена, если отмечен любой её верхний блок: правило с другого устройства тоже видно. */
export function cardBookmarked(
	blocks: readonly { id: string; parentId: string | null }[],
	has: (id: string) => boolean,
): boolean {
	return blocks.some((block) => block.parentId === null && has(block.id));
}

/** Кто поставил сообщение: владелец или агент, название которого лента уже показывает. */
export function bookmarkWho(
	entry: Pick<BookmarkView, 'role' | 'agent' | 'harness'>,
): string {
	if (entry.role === 'owner') return 'Вы';
	return agentTitle(entry.agent ?? '', {
		title: entry.harness,
		provider: harnessProvider(entry.harness),
	});
}

export function bookmarkExcerpt(entry: Pick<BookmarkView, 'excerpt'>): string {
	const text = entry.excerpt.trim();
	return text || 'Без текста';
}

/** Дата и время самого сообщения, а не закладки. */
export function bookmarkStamp(
	entry: Pick<BookmarkView, 'target_created_at'>,
	now: number,
): string {
	const at = Date.parse(entry.target_created_at);
	return Number.isFinite(at) ? fmt.stamp(at, now) : '';
}

/** Переход к закладке — тот же адрес, что у совпадения поиска. */
export function bookmarkHref(
	entry: Pick<BookmarkView, 'conversation_id' | 'block_id' | 'input_id' | 'jump_seq'>,
): string {
	return hitHref(entry);
}

export function normalizeBookmark(entry: BookmarkEventView | BookmarkView): BookmarkView {
	return { ...entry, seq: Number(entry.seq), jump_seq: Number(entry.jump_seq) };
}

/** Закладки разговора в порядке ленты. */
export function timelineOrder(items: readonly BookmarkView[]): BookmarkView[] {
	return [...items].sort((a, b) => a.seq - b.seq || a.target_id.localeCompare(b.target_id));
}

export function upsertBookmark(
	items: readonly BookmarkView[],
	entry: BookmarkView,
): BookmarkView[] {
	return timelineOrder([
		...items.filter((item) => bookmarkTarget(item) !== bookmarkTarget(entry)),
		entry,
	]);
}

export function removeBookmark(
	items: readonly BookmarkView[],
	kind: BookmarkTargetKind,
	targetId: string,
): BookmarkView[] {
	const key = bookmarkKey(kind, targetId);
	return items.filter((item) => bookmarkTarget(item) !== key);
}

/** Избранное всех разговоров: новые закладки первыми. */
export function newestFirst(items: readonly BookmarkView[]): BookmarkView[] {
	return [...items].sort(
		(a, b) =>
			Date.parse(b.created_at) - Date.parse(a.created_at) ||
			b.id.localeCompare(a.id),
	);
}

/** Подпись инструмента в записи для доступных имён строк. */
export function bookmarkHarness(entry: Pick<BookmarkView, 'harness'>): string {
	return harnessLabel({ title: entry.harness, provider: harnessProvider(entry.harness) });
}
