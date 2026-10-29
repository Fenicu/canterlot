import type { BlockModel } from './blocks';
import type { InputView } from './inputs';
import { rawCell } from './cell.svelte';

export type TimelineEntry =
	| { kind: 'input'; key: string; seq: string; input: InputView }
	| { kind: 'block'; key: string; seq: string; block: BlockModel };

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
		const without = this.entries.filter((item) => item.key !== entry.key);
		without.push(entry);
		without.sort((a, b) => cmpSeq(a.seq, b.seq));
		if (without.length > this.maxLoaded) {
			without.splice(0, without.length - this.maxLoaded);
			this.hasOlder = true;
		}
		this.#entries.set(without);
	}

	remove(key: string): void {
		this.#entries.set(this.entries.filter((item) => item.key !== key));
	}

	newestSeq(): string | null {
		const last = this.entries[this.entries.length - 1];
		return last?.seq ?? null;
	}

	oldestSeq(): string | null {
		return this.entries[0]?.seq ?? null;
	}
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
