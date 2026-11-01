import type { ChatStore } from '#lib/stores/chat.svelte.js';
import { SvelteSet } from 'svelte/reactivity';
import { rawCell } from '#lib/stores/cell.svelte.js';
import { cmpSeq, type TimelineEntry, type TimelineSource } from '#lib/stores/timeline.js';

/** Записи ленты разговора и подгруженной ветки по возрастанию номера, без повторов. */
export function mergeLaneEntries(live: readonly TimelineEntry[], fetched: readonly TimelineEntry[]): TimelineEntry[] {
	if (fetched.length === 0) return live as TimelineEntry[];
	const seen = new SvelteSet<string>();
	const out: TimelineEntry[] = [];
	for (const entry of [...live, ...fetched]) {
		if (seen.has(entry.key)) continue;
		seen.add(entry.key);
		out.push(entry);
	}
	return out.sort((a, b) => cmpSeq(a.seq, b.seq));
}

export type LaneLoad = 'idle' | 'loading' | 'ready' | 'failed';

/**
 * Лента одного субагента. Окно ленты разговора держит только хвост, ветка старого субагента
 * в нём не лежит, поэтому её страницы берутся у хаба отдельно и лента разговора не меняется.
 * Живые блоки приходят в общую карту событиями, как и раньше.
 */
export class LaneFeed implements TimelineSource {
	hasOlder = $state(false);
	load = $state<LaneLoad>('idle');
	readonly #fetched = rawCell<TimelineEntry[]>([]);
	readonly entries = $derived.by(() => mergeLaneEntries(this.chat.timeline.entries, [...this.chat.laneLive, ...this.#fetched.get()]));
	#before: string | null = null;
	#busy = false;
	#unwatch: (() => void) | null = null;

	constructor(
		readonly chat: ChatStore,
		readonly rootId: string
	) {}

	/** Корень найден: либо в ленте разговора, либо в подгруженной ветке. */
	get root() {
		return this.chat.blocks.get(this.rootId);
	}

	async start(): Promise<void> {
		this.#unwatch ??= this.chat.watchLane(this.rootId);
		await this.#page(true);
	}

	stop(): void {
		this.#unwatch?.();
		this.#unwatch = null;
	}

	async loadOlder(): Promise<void> {
		if (!this.hasOlder) return;
		await this.#page(false);
	}

	async #page(first: boolean): Promise<void> {
		if (this.#busy) return;
		this.#busy = true;
		if (first && this.load !== 'ready') this.load = 'loading';
		try {
			const page = await this.chat.loadLane(this.rootId, first ? undefined : (this.#before ?? undefined));
			this.#fetched.set([...this.#fetched.get(), ...page.entries]);
			// Повторный запрос хвоста не сбрасывает курсор уже прочитанных старых страниц.
			if (!first || this.load !== 'ready') {
				this.#before = page.nextBefore;
				this.hasOlder = page.nextBefore !== null;
			}
			this.load = 'ready';
		} catch {
			if (this.load !== 'ready') this.load = 'failed';
		} finally {
			this.#busy = false;
		}
	}
}
