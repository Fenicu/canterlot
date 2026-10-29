import { utf8Length, utf8Slice, type BlockPage } from '../../src/lib/stores/blocks';
import type { HistoryItem, HistoryPage, SnapshotData } from '../../src/lib/stores/chat.svelte';
import type { DelegationNode } from '../../src/lib/stores/delegation';
import type { Counts, ListRow, ListSnapshot } from '../../src/lib/stores/list.svelte';
import type { ProviderLimits } from '../../src/lib/stores/limits';
import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import type { RunInfo } from '../../src/lib/stores/run';
import type { RangeResult } from '../../src/lib/sync/scope';
import type { FakeHub } from './hub';

const PAGE = 256 * 1024;

export interface CanonicalBlock {
	text: string;
	rev: number;
	state: string;
}

export class FakeHubApi {
	readonly blocks = new Map<string, CanonicalBlock>();
	readonly histories = new Map<string, HistoryItem[]>();
	readonly details = new Map<string, Record<string, unknown>>();
	readonly list = new Map<string, ListRow>();
	readonly delegations = new Map<string, DelegationNode[]>();
	readonly runs = new Map<string, RunInfo>();
	readonly machines = new Map<string, MachineRecord>();
	readonly reads: { id: string; seq: string }[] = [];
	readonly decisions = new Map<string, { winnerId: string; decision: unknown; key: string }>();
	groups: ListSnapshot['groups'] = [];
	counts: Counts = { all: 0, pinned: 0, archived: 0, groups: [] };
	limits: ProviderLimits[] = [];
	truncated = false;
	decisionAutoProgress: boolean | 'transmitting' | 'saved' = true;
	#seq = 0;

	constructor(readonly hub: FakeHub) {
		this.hub.setSnapshot('conversations', () => this.listData());
		this.hub.setSnapshot('machines', () => ({ items: [...this.machines.values()] }));
	}

	id(): string {
		this.#seq += 1;
		return `00000000-0000-4000-8000-${this.#seq.toString(16).padStart(12, '0')}`;
	}

	bindConversation(id: string): void {
		this.hub.setSnapshot(`conversation:${id}`, () => this.conversationData(id));
	}

	conversationData(id: string): SnapshotData {
		const requestsMap = new Map<string, Record<string, unknown>>();

		const rawDetail = this.details.get(id) as Record<string, unknown> | undefined;
		if (Array.isArray(rawDetail?.open_requests)) {
			for (const r of rawDetail.open_requests as Record<string, unknown>[]) {
				const reqId = String(r.id ?? r.request_id ?? '');
				if (reqId) requestsMap.set(reqId, { ...r, id: reqId, request_id: reqId });
			}
		}

		for (const ev of this.hub.log(`conversation:${id}`)) {
			const p = ev.payload as Record<string, unknown>;
			const reqId = String(p?.request_id ?? p?.id ?? '');
			if (!reqId) continue;
			if (ev.kind === 'request_opened') {
				const existing = requestsMap.get(reqId);
				if (!existing) {
					requestsMap.set(reqId, {
						id: reqId,
						request_id: reqId,
						conversation_id: id,
						kind: p.kind ?? 'question',
						state: 'open',
						request_payload: p.payload ?? p,
						payload: p.payload ?? p
					});
				}
			} else if (ev.kind === 'request_decided') {
				const req = requestsMap.get(reqId);
				if (req) {
					req.state = 'decision_saved';
					req.decision_payload = p.decision ?? p.winner_decision;
					req.winner_decision = p.decision ?? p.winner_decision;
				}
			} else if (ev.kind === 'request_transmitting') {
				const req = requestsMap.get(reqId);
				if (req && (req.state === 'open' || req.state === 'decision_saved')) {
					req.state = 'decision_transmitting';
				}
			} else if (ev.kind === 'request_closed') {
				const req = requestsMap.get(reqId);
				if (req) {
					const reason = String(p.reason ?? 'closed');
					req.state = reason === 'applied' ? 'decision_applied' : (reason === 'not_applied' ? 'decision_not_applied' : 'closed_without_decision');
					req.reason = reason;
				}
			}
		}

		const open_requests = [...requestsMap.values()].filter((r) =>
			r.state === 'open' || r.state === 'decision_saved' || r.state === 'decision_transmitting'
		);

		return {
			detail: this.details.get(id) ?? { id, title: 'Сессия' },
			queue: (this.details.get(id)?.queue as SnapshotData['queue']) ?? { pause_reasons: [], hold_reasons: [], queued: 0, run_slot: 'free' },
			history: this.page(id, {}),
			scheduled: [],
			open_requests
		};
	}

	listData(): ListSnapshot {
		return {
			counts: this.counts,
			groups: this.groups,
			items: [...this.list.values()],
			limits: this.limits,
			truncated: this.truncated
		};
	}

	async conversationSnapshot(id: string): Promise<{ cursor: string; data: SnapshotData }> {
		const snap = this.hub.snapshot(`conversation:${id}`);
		return { cursor: snap.cursor, data: (snap.data as SnapshotData | null) ?? this.conversationData(id) };
	}

	async history(id: string, query: { before?: string; after?: string; limit?: number } = {}): Promise<HistoryPage> {
		return this.page(id, query);
	}

	page(id: string, query: { before?: string; after?: string; limit?: number }): HistoryPage {
		const all = [...(this.histories.get(id) ?? [])].sort((a, b) => cmp(seqOf(a), seqOf(b)));
		let filtered = all;
		if (query.before) filtered = filtered.filter((item) => cmp(seqOf(item), query.before!) < 0);
		if (query.after) filtered = filtered.filter((item) => cmp(seqOf(item), query.after!) > 0);
		const limit = query.limit ?? 100;
		const slice = query.after ? filtered.slice(0, limit) : filtered.slice(-limit);
		const oldest = slice[0];
		const newest = slice[slice.length - 1];
		const nextBefore = oldest && all.some((item) => cmp(seqOf(item), seqOf(oldest)) < 0) ? seqOf(oldest) : null;
		const nextAfter = newest && all.some((item) => cmp(seqOf(item), seqOf(newest)) > 0) ? seqOf(newest) : null;
		return { items: [...slice].reverse(), next_before: nextBefore, next_after: nextAfter };
	}

	async block(id: string, blockId: string, offset: number): Promise<BlockPage> {
		void id;
		const record = this.blocks.get(blockId);
		if (!record) throw new Error('not_found');
		const tail = utf8Slice(record.text, offset);
		const total = utf8Length(record.text);
		let content = tail;
		let next = total;
		if (utf8Length(tail) > PAGE) {
			content = takeBytes(tail, PAGE);
			next = offset + utf8Length(content);
		}
		return {
			content,
			offset,
			next_offset: next >= total ? null : next,
			rev: record.rev,
			state: record.state,
			total_bytes: total,
			checksum: null
		};
	}

	async markRead(id: string, seq: string): Promise<void> {
		this.reads.push({ id, seq });
	}

	async events(scope: string, after: string, limit: number): Promise<RangeResult> {
		return this.hub.range(scope, after, limit);
	}

	async listSnapshot(): Promise<{ cursor: string; data: ListSnapshot }> {
		const snap = this.hub.snapshot('conversations');
		return { cursor: snap.cursor, data: (snap.data as ListSnapshot | null) ?? this.listData() };
	}

	async machinesSnapshot(): Promise<{ cursor: string; data: { items: MachineRecord[] } }> {
		const snap = this.hub.snapshot('machines');
		return { cursor: snap.cursor, data: (snap.data as { items: MachineRecord[] } | null) ?? { items: [...this.machines.values()] } };
	}

	async delegation(conversationId: string): Promise<DelegationNode[]> {
		return this.delegations.get(conversationId) ?? [];
	}

	async run(runId: string): Promise<RunInfo> {
		const info = this.runs.get(runId);
		if (!info) throw new Error('not_found');
		return info;
	}
}

function seqOf(item: HistoryItem): string {
	return item.kind === 'block' ? item.created_seq : item.feed_seq || item.created_seq;
}

function cmp(a: string, b: string): number {
	const left = BigInt(a);
	const right = BigInt(b);
	return left < right ? -1 : left > right ? 1 : 0;
}

function takeBytes(value: string, max: number): string {
	let low = 0;
	let high = value.length;
	while (low < high) {
		const mid = Math.ceil((low + high) / 2);
		if (utf8Length(value.slice(0, mid)) <= max) low = mid;
		else high = mid - 1;
	}
	return value.slice(0, low);
}
