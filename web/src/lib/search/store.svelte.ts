import { ApiError } from '#lib/api/http.js';
import { errorText } from '#lib/api/errors.js';
import {
	emptyState,
	searchBody,
	type KindFilter,
	type Period,
	type SearchBody,
	type SearchHit,
	type SearchPage,
	type SearchState,
	type SessionScope
} from './model';

export type SearchFetch = (body: SearchBody, signal: AbortSignal) => Promise<SearchPage>;

export type SearchStatus = 'idle' | 'loading' | 'ready' | 'error';

/** Сбой страницы: таймаут хаба отдельно, чтобы подсказать сузить запрос. */
export interface SearchFailure {
	kind: 'timeout' | 'failed';
	message: string;
}

function failureOf(err: unknown): SearchFailure {
	if (err instanceof ApiError && err.status === 503) {
		return { kind: 'timeout', message: 'Поиск не уложился во время. Уточните запрос или сузьте период' };
	}
	return { kind: 'failed', message: errorText(err, 'Не удалось выполнить поиск') };
}

/**
 * Поиск по истории: запрос, фильтры и страницы совпадений по курсору.
 * Каждая смена запроса или фильтра начинает поиск заново; ответ устаревшего
 * запроса отбрасывается, а не смешивается с новым.
 */
export class SearchStore {
	q = $state('');
	period = $state<Period>('all');
	kind = $state<KindFilter>('all');
	session = $state<SessionScope | null>(null);

	hits = $state<SearchHit[]>([]);
	total = $state(0);
	capped = $state(false);
	cursor = $state<string | null>(null);
	status = $state<SearchStatus>('idle');
	failure = $state<SearchFailure | null>(null);
	loadingMore = $state(false);
	moreFailure = $state<SearchFailure | null>(null);

	#generation = 0;
	#controller: AbortController | null = null;
	readonly #fetch: SearchFetch;
	readonly #now: () => number;

	constructor(fetch: SearchFetch, initial: SearchState = emptyState(), now: () => number = Date.now) {
		this.#fetch = fetch;
		this.#now = now;
		this.restore(initial);
	}

	get state(): SearchState {
		return { q: this.q, period: this.period, kind: this.kind, session: this.session };
	}

	get query(): string {
		return this.q.trim();
	}

	get hasMore(): boolean {
		return this.cursor !== null;
	}

	restore(state: SearchState): void {
		this.q = state.q;
		this.period = state.period;
		this.kind = state.kind;
		this.session = state.session;
	}

	/** Пустой запрос ничего не ищет: список очищается без похода в хаб. */
	async run(): Promise<void> {
		const generation = ++this.#generation;
		this.#controller?.abort();
		this.#controller = null;
		this.loadingMore = false;
		this.moreFailure = null;
		this.failure = null;
		if (!this.query) {
			this.hits = [];
			this.total = 0;
			this.capped = false;
			this.cursor = null;
			this.status = 'idle';
			return;
		}
		const controller = new AbortController();
		this.#controller = controller;
		this.status = 'loading';
		try {
			const page = await this.#fetch(searchBody(this.state, this.#now()), controller.signal);
			if (generation !== this.#generation) return;
			this.hits = page.hits;
			this.total = page.total;
			this.capped = page.total_capped;
			this.cursor = page.next_cursor ?? null;
			this.status = 'ready';
		} catch (err) {
			if (generation !== this.#generation) return;
			this.hits = [];
			this.cursor = null;
			this.failure = failureOf(err);
			this.status = 'error';
		} finally {
			if (this.#controller === controller) this.#controller = null;
		}
	}

	/** Следующая страница по курсору; повторный вызов во время загрузки ничего не делает. */
	async more(): Promise<void> {
		const cursor = this.cursor;
		if (!cursor || this.loadingMore || this.status !== 'ready') return;
		const generation = this.#generation;
		this.loadingMore = true;
		this.moreFailure = null;
		const controller = new AbortController();
		this.#controller = controller;
		try {
			const page = await this.#fetch(searchBody(this.state, this.#now(), cursor), controller.signal);
			if (generation !== this.#generation) return;
			const known = this.hits.map(hitKey);
			this.hits = [...this.hits, ...page.hits.filter((hit) => !known.includes(hitKey(hit)))];
			this.total = page.total;
			this.capped = page.total_capped;
			this.cursor = page.next_cursor ?? null;
		} catch (err) {
			if (generation !== this.#generation) return;
			this.moreFailure = failureOf(err);
		} finally {
			if (generation === this.#generation) this.loadingMore = false;
			if (this.#controller === controller) this.#controller = null;
		}
	}

	dispose(): void {
		this.#generation += 1;
		this.#controller?.abort();
		this.#controller = null;
	}
}

export function hitKey(hit: Pick<SearchHit, 'kind' | 'block_id' | 'input_id' | 'conversation_id' | 'seq'>): string {
	return `${hit.kind}:${hit.block_id ?? hit.input_id ?? `${hit.conversation_id}:${hit.seq}`}`;
}
