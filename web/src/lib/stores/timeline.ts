import type { BlockModel } from './blocks';
import type { InputView } from './inputs';
import type { RequestItem } from './requests.svelte';
import { rawCell } from './cell.svelte';

export type TimelineEntry =
	| { kind: 'input'; key: string; seq: string; input: InputView }
	| { kind: 'block'; key: string; seq: string; block: BlockModel }
	| { kind: 'request'; key: string; seq: string; request: RequestItem };

/** Порядковый номер сообщения, которого хаб ещё не принял в ленту: оно стоит в самом конце. */
export const LOCAL_SEQ = '9223372036854775807';

export function isLocalEntry(entry: TimelineEntry): boolean {
	return entry.seq === LOCAL_SEQ;
}

export function cmpSeq(a: string, b: string): number {
	const left = BigInt(a);
	const right = BigInt(b);
	if (left < right) return -1;
	if (left > right) return 1;
	return 0;
}

export class TimelineModel {
	readonly maxLoaded = 3000;
	hasOlder = false;
	tailDetached = false;
	newerCount = 0;
	#entries = rawCell<TimelineEntry[]>([]);

	get entries(): TimelineEntry[] {
		return this.#entries.get();
	}
	clear(): void { this.#entries.set([]); this.hasOlder = false; this.tailDetached = false; this.newerCount = 0; }

	applyHistoryPage(items: TimelineEntry[], dir: 'older' | 'newer' | 'tail'): void {
		const locals = this.entries.filter(isLocalEntry);
		this.#applyPage(items, dir);
		if (locals.length === 0) return;
		const present = new Set(this.entries.map((entry) => entry.key));
		const missing = locals.filter((entry) => !present.has(entry.key));
		if (missing.length > 0) this.#entries.set([...this.entries, ...missing]);
	}

	#applyPage(items: TimelineEntry[], dir: 'older' | 'newer' | 'tail'): void {
		this.#entries.set(this.entries.filter((entry) => !isLocalEntry(entry)));
		const incoming = [...items].sort((a, b) => cmpSeq(a.seq, b.seq));
		if (dir === 'tail') {
			const oldest = incoming[0]?.seq;
			const kept = oldest ? this.entries.filter((entry) => cmpSeq(entry.seq, oldest) < 0) : [];
			const combined = dedupe([...kept, ...incoming]);
			if (combined.length > this.maxLoaded) {
				combined.splice(0, combined.length - this.maxLoaded);
				this.hasOlder = true;
			}
			this.#entries.set(combined);
			this.tailDetached = false;
			this.newerCount = 0;
			return;
		}
		const merged =
			dir === 'older' ? dedupe([...incoming, ...this.entries]) : dedupe([...this.entries, ...incoming]);
		if (merged.length > this.maxLoaded) {
			if (dir === 'newer') {
				merged.splice(0, merged.length - this.maxLoaded);
				this.hasOlder = true;
			} else {
				merged.splice(this.maxLoaded);
				this.tailDetached = true;
			}
		}
		this.#entries.set(merged);
	}

	trimNewestEnd(): void {
		if (this.entries.length <= this.maxLoaded) return;
		this.#entries.set(this.entries.slice(0, this.maxLoaded));
		this.tailDetached = true;
	}

	upsert(entry: TimelineEntry): void {
		if (this.tailDetached) {
			this.newerCount += 1;
			return;
		}
		const current = this.entries.find((item) => item.key === entry.key);
		// Новый массив пересобирает все строки ленты: повтор той же записи (accepted после transmitting) его не создаёт.
		if (current && sameEntry(current, entry)) return;
		const without = current ? this.entries.filter((item) => item !== current) : [...this.entries];
		const last = without[without.length - 1];
		without.push(entry);
		if (last && cmpSeq(last.seq, entry.seq) > 0) without.sort((a, b) => cmpSeq(a.seq, b.seq));
		if (without.length > this.maxLoaded) {
			without.splice(0, without.length - this.maxLoaded);
			this.hasOlder = true;
		}
		this.#entries.set(without);
	}

	remove(key: string): void {
		if (!this.entries.some((item) => item.key === key)) return;
		this.#entries.set(this.entries.filter((item) => item.key !== key));
	}

	newestSeq(): string | null {
		return this.entries.findLast((entry) => !isLocalEntry(entry))?.seq ?? null;
	}

	oldestSeq(): string | null {
		return this.entries.find((entry) => !isLocalEntry(entry))?.seq ?? null;
	}
}

function sameEntry(a: TimelineEntry, b: TimelineEntry): boolean {
	if (a.seq !== b.seq) return false;
	if (a.kind === 'block') return b.kind === 'block' && a.block === b.block;
	if (a.kind === 'request') return b.kind === 'request' && a.request === b.request;
	return b.kind === 'input' && a.input === b.input;
}

function dedupe(items: TimelineEntry[]): TimelineEntry[] {
	const seen = new Set<string>();
	const out: TimelineEntry[] = [];
	for (const item of items) {
		if (seen.has(item.key)) continue;
		seen.add(item.key);
		out.push(item);
	}
	return out;
}
