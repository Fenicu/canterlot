import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { deleteBookmark, listBookmarks, listConversationBookmarks, putBookmark } from '#lib/api/client.js';
import { errorText } from '#lib/api/errors.js';
import {
	bookmarkKey,
	bookmarkTarget,
	newestFirst,
	normalizeBookmark,
	removeBookmark,
	timelineOrder,
	upsertBookmark,
	type BookmarkChange,
	type BookmarkPage,
	type BookmarkTargetKind,
	type BookmarkView
} from './model.js';

export interface BookmarkApi {
	listConversation(conversationId: string): Promise<BookmarkView[]>;
	put(conversationId: string, kind: BookmarkTargetKind, targetId: string): Promise<BookmarkView>;
	remove(conversationId: string, kind: BookmarkTargetKind, targetId: string): Promise<void>;
}

export interface BookmarksPageApi {
	list(query: { limit: number; cursor?: string }): Promise<BookmarkPage>;
}

export interface BookmarksListApi extends BookmarksPageApi {
	remove(conversationId: string, kind: BookmarkTargetKind, targetId: string): Promise<void>;
}

/** Закладки одного разговора: загружаются при открытии чата и живут, пока он открыт. */
export class ConversationBookmarks {
	readonly conversationId: string;
	items = $state<BookmarkView[]>([]);
	loaded = $state(false);
	loading = $state(false);
	loadError = $state<string | null>(null);
	/** Ошибка последнего переключения: строка в чате, как у прочих действий. */
	toggleError = $state<string | null>(null);
	/** Отмеченные цели: множество меняется сразу, до ответа хаба. */
	readonly targets = new SvelteSet<string>();
	readonly count = $derived(this.targets.size);
	readonly list = $derived(timelineOrder(this.items));

	#generation = 0;
	#intent = new SvelteMap<string, boolean>();
	#queues = new SvelteMap<string, Promise<void>>();
	readonly #api: BookmarkApi;

	constructor(conversationId: string, api: BookmarkApi) {
		this.conversationId = conversationId;
		this.#api = api;
	}

	has(kind: BookmarkTargetKind, targetId: string): boolean {
		return this.targets.has(bookmarkKey(kind, targetId));
	}

	async load(): Promise<void> {
		if (this.loading) return;
		const generation = ++this.#generation;
		this.loading = true;
		this.loadError = null;
		try {
			const entries = await this.#api.listConversation(this.conversationId);
			if (generation !== this.#generation) return;
			this.replace(entries);
			this.loaded = true;
		} catch (err) {
			if (generation !== this.#generation) return;
			this.loadError = errorText(err, 'Не удалось загрузить избранное');
		} finally {
			if (generation === this.#generation) this.loading = false;
		}
	}

	async toggle(kind: BookmarkTargetKind, targetId: string): Promise<void> {
		const key = bookmarkKey(kind, targetId);
		const desired = !this.targets.has(key);
		this.#intent.set(key, desired);
		this.toggleError = null;
		const prior = this.items.find((item) => bookmarkTarget(item) === key) ?? null;
		this.#setLocal(key, desired, prior);
		const run = async () => {
			// Следующее нажатие уже выбрало другое состояние: этот запрос ничего не значит.
			if (this.#intent.get(key) !== desired) return;
			try {
				if (desired) {
					const entry = await this.#api.put(this.conversationId, kind, targetId);
					if (this.#intent.get(key) !== desired) return;
					this.targets.add(key);
					this.items = upsertBookmark(this.items, entry);
				} else {
					await this.#api.remove(this.conversationId, kind, targetId);
					if (this.#intent.get(key) !== desired) return;
					this.targets.delete(key);
					this.items = removeBookmark(this.items, kind, targetId);
				}
			} catch (err) {
				if (this.#intent.get(key) !== desired) return;
				this.#setLocal(key, !desired, prior);
				this.toggleError = errorText(err, 'Не удалось изменить избранное');
			}
		};
		const previous = this.#queues.get(key) ?? Promise.resolve();
		const next = previous.then(run, run);
		this.#queues.set(key, next);
		try {
			await next;
		} finally {
			if (this.#queues.get(key) === next) {
				this.#queues.delete(key);
				this.#intent.delete(key);
			}
		}
	}

	remove(kind: BookmarkTargetKind, targetId: string): Promise<void> {
		if (!this.has(kind, targetId)) return Promise.resolve();
		return this.toggle(kind, targetId);
	}

	apply(change: BookmarkChange): void {
		if (change.conversation_id !== this.conversationId) return;
		const key = bookmarkKey(change.target_kind, change.target_id);
		if (change.bookmarked) {
			this.targets.add(key);
			if (change.bookmark) {
				this.items = upsertBookmark(this.items, normalizeBookmark(change.bookmark));
			}
		} else {
			this.targets.delete(key);
			this.items = removeBookmark(this.items, change.target_kind, change.target_id);
		}
	}

	replace(entries: BookmarkView[]): void {
		this.items = timelineOrder(entries);
		this.targets.clear();
		for (const entry of entries) this.targets.add(bookmarkTarget(entry));
	}

	#setLocal(key: string, on: boolean, prior: BookmarkView | null): void {
		if (on) {
			this.targets.add(key);
			if (prior) this.items = upsertBookmark(this.items, prior);
		} else {
			this.targets.delete(key);
			this.items = this.items.filter((item) => bookmarkTarget(item) !== key);
		}
	}
}

type ChangeListener = (change: BookmarkChange) => void;
type DropListener = (conversationId: string) => void;

const changeListeners = new SvelteSet<ChangeListener>();
const dropListeners = new SvelteSet<DropListener>();

export function onBookmarkChange(listener: ChangeListener): () => void {
	changeListeners.add(listener);
	return () => changeListeners.delete(listener);
}

export function onBookmarkDrop(listener: DropListener): () => void {
	dropListeners.add(listener);
	return () => dropListeners.delete(listener);
}

/** Открытые чаты: закладки загружаются при входе в разговор и снимаются при выходе. */
export class SessionBookmarks {
	readonly models = new SvelteMap<string, ConversationBookmarks>();
	#refs = new SvelteMap<string, number>();
	readonly #api: BookmarkApi;

	constructor(api: BookmarkApi) {
		this.#api = api;
	}

	get(conversationId: string): ConversationBookmarks | null {
		return this.models.get(conversationId) ?? null;
	}

	open(conversationId: string): ConversationBookmarks {
		this.#refs.set(conversationId, (this.#refs.get(conversationId) ?? 0) + 1);
		let model = this.models.get(conversationId);
		if (!model) {
			model = new ConversationBookmarks(conversationId, this.#api);
			this.models.set(conversationId, model);
			void model.load();
		}
		return model;
	}

	close(conversationId: string): void {
		const left = (this.#refs.get(conversationId) ?? 1) - 1;
		if (left > 0) {
			this.#refs.set(conversationId, left);
			return;
		}
		this.#refs.delete(conversationId);
		this.models.delete(conversationId);
	}

	apply(change: BookmarkChange): void {
		this.models.get(change.conversation_id)?.apply(change);
	}

	drop(conversationId: string): void {
		this.models.delete(conversationId);
		this.#refs.delete(conversationId);
	}
}

const api: BookmarkApi = {
	async listConversation(conversationId) {
		const response = await listConversationBookmarks(conversationId);
		return response.data ?? [];
	},
	async put(conversationId, kind, targetId) {
		const response = await putBookmark(conversationId, kind, targetId);
		return response.data;
	},
	async remove(conversationId, kind, targetId) {
		await deleteBookmark(conversationId, kind, targetId);
	}
};

export const sessionBookmarks = new SessionBookmarks(api);

/** Одно событие хаба: сначала открытые чаты, затем экраны со своими списками. */
export function applyBookmarkChange(change: BookmarkChange): void {
	sessionBookmarks.apply(change);
	for (const listener of [...changeListeners]) listener(change);
}

export function dropConversationBookmarks(conversationId: string): void {
	sessionBookmarks.drop(conversationId);
	for (const listener of [...dropListeners]) listener(conversationId);
}

export const BOOKMARKS_PAGE = 20;

/**
 * Избранное всех разговоров: страницы по курсору. Список живёт на экране `/bookmarks`
 * и обновляется событием, пока открыт.
 */
export class BookmarksListStore {
	items = $state<BookmarkView[]>([]);
	cursor = $state<string | null>(null);
	status = $state<'idle' | 'loading' | 'ready' | 'error'>('idle');
	failure = $state<string | null>(null);
	loadingMore = $state(false);
	moreFailure = $state<string | null>(null);
	actionError = $state<string | null>(null);

	#paged = false;
	#generation = 0;
	#unsubscribe: () => void;
	readonly #api: BookmarksListApi;

	constructor(api: BookmarksListApi, subscribe = onBookmarkChange) {
		this.#api = api;
		this.#unsubscribe = subscribe((change) => this.apply(change));
	}

	get hasMore(): boolean {
		return this.cursor !== null;
	}

	async run(): Promise<void> {
		const generation = ++this.#generation;
		this.status = 'loading';
		this.failure = null;
		this.moreFailure = null;
		this.#paged = false;
		try {
			const page = await this.#api.list({ limit: BOOKMARKS_PAGE });
			if (generation !== this.#generation) return;
			this.items = newestFirst(page.bookmarks);
			this.cursor = page.next_cursor ?? null;
			this.status = 'ready';
		} catch (err) {
			if (generation !== this.#generation) return;
			this.items = [];
			this.cursor = null;
			this.failure = errorText(err, 'Не удалось загрузить избранное');
			this.status = 'error';
		}
	}

	async more(): Promise<void> {
		const cursor = this.cursor;
		if (!cursor || this.loadingMore || this.status !== 'ready') return;
		this.loadingMore = true;
		this.moreFailure = null;
		this.#paged = true;
		const generation = this.#generation;
		try {
			const page = await this.#api.list({ limit: BOOKMARKS_PAGE, cursor });
			if (generation !== this.#generation) return;
			const known = new SvelteSet(this.items.map((item) => item.id));
			this.items = newestFirst([...this.items, ...page.bookmarks.filter((item) => !known.has(item.id))]);
			this.cursor = page.next_cursor ?? null;
		} catch (err) {
			if (generation !== this.#generation) return;
			this.moreFailure = errorText(err, 'Не удалось загрузить ещё');
		} finally {
			if (generation === this.#generation) this.loadingMore = false;
		}
	}

	apply(change: BookmarkChange): void {
		const key = `${change.target_kind}:${change.target_id}`;
		if (!change.bookmarked) {
			this.items = this.items.filter(
				(item) => `${item.target_kind}:${item.target_id}` !== key
			);
			return;
		}
		// Пока список не перелистнули, новая закладка встаёт наверх сразу.
		if (this.#paged || this.status !== 'ready' || !change.bookmark) return;
		this.items = newestFirst([
			normalizeBookmark(change.bookmark),
			...this.items.filter((item) => `${item.target_kind}:${item.target_id}` !== key)
		]);
	}

	async remove(entry: BookmarkView): Promise<void> {
		const key = bookmarkTarget(entry);
		const prior = this.items;
		this.actionError = null;
		this.items = this.items.filter((item) => bookmarkTarget(item) !== key);
		try {
			await this.#api.remove(entry.conversation_id, entry.target_kind, entry.target_id);
		} catch (err) {
			this.items = prior;
			this.actionError = errorText(err, 'Не удалось убрать из избранного');
		}
	}

	dispose(): void {
		this.#generation += 1;
		this.#unsubscribe();
	}
}

export const bookmarksPageApi: BookmarksListApi = {
	async list(query) {
		const response = await listBookmarks(query);
		return response.data;
	},
	async remove(conversationId, kind, targetId) {
		await deleteBookmark(conversationId, kind, targetId);
	}
};
