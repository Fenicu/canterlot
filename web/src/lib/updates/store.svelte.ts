import {
	compareUpdates,
	isNewer,
	newestDismissible,
	readDismiss,
	writeDismiss,
	type DismissMarker,
	type DismissStorage
} from '#lib/updates/dismiss.js';
import { toUpdateItem, type UpdateItem } from '#lib/updates/text.js';
import { SvelteMap } from 'svelte/reactivity';

export interface UpdatesQuery {
	limit: number;
	cursor?: string;
	machine?: string;
	kind?: string;
}

export interface UpdatesDeps {
	api: (query: UpdatesQuery) => Promise<{ data: unknown }>;
	storage: DismissStorage;
	ownerId: string;
}

/** Лента обновлений: страницы из API, живые записи из потока машин и закрытие на устройстве. */
export class UpdatesStore {
	items = $state<UpdateItem[]>([]);
	dismissed = $state<DismissMarker | null>(null);
	nextCursor = $state<string | null>(null);
	loading = $state(false);
	error = $state('');

	#phase = '';
	#busy = false;
	/** Записи, пришедшие живым событием после последнего чтения ленты. */
	#live = new SvelteMap<string, UpdateItem>();

	constructor(private deps: UpdatesDeps) {
		this.dismissed = readDismiss(deps.ownerId, deps.storage);
	}

	get unseen(): UpdateItem[] {
		return this.items.filter((item) => isNewer(item, this.dismissed));
	}

	get machines(): string[] {
		const names: string[] = [];
		for (const item of this.items) if (!names.includes(item.machine)) names.push(item.machine);
		return names.sort((a, b) => a.localeCompare(b, 'ru'));
	}

	/** Живое событие: новая запись встаёт в порядок ленты, повтор по id не добавляется. */
	push(item: UpdateItem): void {
		if (!item.id || this.items.some((known) => known.id === item.id)) return;
		this.items = [...this.items, item].sort((a, b) => -compareUpdates(a, b));
		this.#live.set(item.id, item);
	}

	async load(query: { machine?: string; kind?: string } = {}): Promise<void> {
		if (this.#busy) return;
		this.#busy = true;
		this.loading = true;
		this.error = '';
		try {
			const response = await this.deps.api({ limit: 20, ...query });
			const body = response.data as { items?: Record<string, unknown>[]; next_cursor?: string | null };
			const page = (body?.items ?? []).map(toUpdateItem);
			const inPage = page.map((item) => item.id);
			const live = [...this.#live.values()].filter((item) =>
				!inPage.includes(item.id) &&
				(!query.machine || item.machine === query.machine) &&
				(!query.kind || item.kind === query.kind)
			);
			this.items = [...page, ...live].sort((a, b) => -compareUpdates(a, b));
			this.#live.clear();
			this.nextCursor = body?.next_cursor ?? null;
		} catch {
			this.error = 'Не удалось загрузить ленту. Повторите при восстановлении связи';
		} finally {
			this.loading = false;
			this.#busy = false;
		}
	}

	async loadMore(): Promise<void> {
		if (this.#busy || !this.nextCursor) return;
		this.#busy = true;
		this.loading = true;
		this.error = '';
		try {
			const response = await this.deps.api({ limit: 20, cursor: this.nextCursor });
			const body = response.data as { items?: Record<string, unknown>[]; next_cursor?: string | null };
			const page = (body?.items ?? []).map(toUpdateItem);
			const known = this.items.map((item) => item.id);
			for (const item of page) this.#live.delete(item.id);
			this.items = [...this.items, ...page.filter((item) => !known.includes(item.id))];
			this.nextCursor = body?.next_cursor ?? null;
		} catch {
			this.error = 'Не удалось загрузить ленту. Повторите при восстановлении связи';
		} finally {
			this.loading = false;
			this.#busy = false;
		}
	}

	dismiss(): void {
		const marker = newestDismissible(this.items);
		if (!marker) return;
		writeDismiss(this.deps.ownerId, this.deps.storage, marker);
		this.dismissed = marker;
	}

	/** Возврат к живой связи после обрыва: список сверяется с API, а не только с событиями. */
	observePhase(phase: string | undefined): void {
		if (phase === 'live' && this.#phase && this.#phase !== 'live') void this.load();
		this.#phase = phase ?? '';
	}

	reset(): void {
		this.items = [];
		this.nextCursor = null;
		this.dismissed = null;
		this.error = '';
	}
}
