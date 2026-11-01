import { utf8Length, utf8Slice, type BlockPage } from '../../src/lib/stores/blocks';
import type { HistoryBlock, HistoryItem, HistoryPage, LanePage, SnapshotData } from '../../src/lib/stores/chat.svelte';
import type { DelegationNode } from '../../src/lib/stores/delegation';
import type { Counts, ListRow, ListSnapshot } from '../../src/lib/stores/list.svelte';
import type { ProviderLimits } from '../../src/lib/stores/limits';
import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import type { RunInfo } from '../../src/lib/stores/run';
import type { TodoList } from '../../src/lib/stores/todos.svelte';
import type { RangeResult } from '../../src/lib/sync/scope';
import type { HubEvent } from '../../src/lib/sync/wire';
import type { FakeHub } from './hub';
import type { components } from '../../src/lib/api/generated/openapi.js';

type PresetSummary = components['schemas']['PresetSummary'];
type QueueSnapshot = components['schemas']['SnapshotQueueView'];

const WAITING = new Set(['queued', 'dispatched', 'on_machine']);

/** У хаба ввод без места в ленте лежит в очереди снимка, а не в истории. */
function waiting(item: HistoryItem): boolean {
	return item.kind === 'input' && WAITING.has(item.state);
}

const PAGE = 256 * 1024;

export interface CanonicalBlock {
	text: string;
	rev: number;
	state: string;
}

export interface FakePresetOrigin {
	by: string;
	conversation_id: string | null;
	conversation_title: string | null;
	at: string | null;
}

export interface FakePreset {
	id: string;
	kind: 'clean' | 'owner' | 'custom';
	title: string;
	description: string;
	revision: number | null;
	text_bytes: number | null;
	text: string | null;
	created: FakePresetOrigin;
	last_edit: FakePresetOrigin | null;
	used_by: number;
}

export class FakeHubApi {
	readonly blocks = new Map<string, CanonicalBlock>();
	readonly histories = new Map<string, HistoryItem[]>();
	readonly details = new Map<string, Record<string, unknown>>();
	readonly list = new Map<string, ListRow>();
	readonly delegations = new Map<string, DelegationNode[]>();
	readonly runs = new Map<string, RunInfo>();
	readonly machines = new Map<string, MachineRecord>();
	directories = ['/home/owner', '/home/owner/projects', '/srv/canterlot', '/srv/canterlot/web', '/srv/canterlot/.git', '/srv/notes'];
	readonly reads: { id: string; seq: string }[] = [];
	readonly decisions = new Map<string, { winnerId: string; decision: unknown; key: string }>();
	readonly deleted = new Set<string>();
	groups: ListSnapshot['groups'] = [];
	counts: Counts = { all: 0, pinned: 0, archived: 0, groups: [] };
	limits: ProviderLimits[] = [];
	truncated = false;
	presets = new Map<string, FakePreset>();
	/** `null`: встроенный список по умолчанию, как у хаба. */
	spawnWhitelist: { harness: string; model: string }[] | null = null;
	readonly spawnWhitelistKeys: string[] = [];
	/** Вызовы приёма результата: ключ идемпотентности и `force`, как их прислал клиент. */
	readonly acceptCalls: { id: string; force: boolean; key: string }[] = [];
	/** Рабочая копия с правками: приём без `force` оставляет её и переводит в `dirty`. */
	acceptFindsDirtyCopy = false;
	readonly defaultSpawnWhitelist = [
		{ harness: 'claude', model: 'opus' },
		{ harness: 'claude', model: 'default' }
	];
	deletedPresets = new Map<string, FakePreset>();
	decisionAutoProgress: boolean | 'transmitting' | 'saved' = true;
	#seq = 0;

	constructor(readonly hub: FakeHub) {
		this.hub.setSnapshot('conversations', () => this.listData());
		this.hub.setSnapshot('machines', () => ({ items: [...this.machines.values()] }));
	}

	spawnWhitelistView(): { entries: { harness: string; model: string }[]; is_default: boolean; default_entries: { harness: string; model: string }[] } {
		return {
			entries: this.spawnWhitelist ?? this.defaultSpawnWhitelist,
			is_default: this.spawnWhitelist === null,
			default_entries: this.defaultSpawnWhitelist
		};
	}

	/** Как настоящий хаб: строки исчезают, список узнаёт из `conversation_deleted`. */
	deleteConversation(id: string): void {
		const row = this.list.get(id);
		this.list.delete(id);
		this.details.delete(id);
		this.histories.delete(id);
		this.deleted.add(id);
		const rows = [...this.list.values()];
		this.counts = {
			all: rows.filter((item) => !item.archived).length,
			pinned: rows.filter((item) => item.pinned && !item.archived).length,
			archived: rows.filter((item) => item.archived).length,
			groups: this.groups.map((group) => ({
				id: group.id,
				count: rows.filter((item) => item.groups.includes(group.id) && !item.archived).length
			}))
		};
		if (!row) return;
		this.hub.append('conversations', {
			kind: 'conversation_deleted',
			conversation_id: id,
			payload: { id, counts: this.counts }
		});
		this.hub.flush();
	}

	id(): string {
		this.#seq += 1;
		return `00000000-0000-4000-8000-${this.#seq.toString(16).padStart(12, '0')}`;
	}

	bindConversation(id: string): void {
		this.hub.setSnapshot(`conversation:${id}`, () => this.conversationData(id));
	}

	/** Запросы разговора по журналу: снимок берёт незавершённые, история все, каждый со своим местом. */
	private requestRows(id: string): Map<string, Record<string, unknown>> {
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
						payload: p.payload ?? p,
						created_seq: ev.scope_seq,
						created_at: ev.committed_at
					});
				} else {
					existing.created_seq ??= ev.scope_seq;
					existing.created_at ??= ev.committed_at;
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

		return requestsMap;
	}

	conversationData(id: string): SnapshotData {
		const open_requests = [...this.requestRows(id).values()].filter((r) =>
			r.state === 'open' || r.state === 'decision_saved' || r.state === 'decision_transmitting'
		);
		const background = new Map<string, BackgroundTask>();
		for (const ev of this.hub.log(`conversation:${id}`)) {
			if (ev.kind !== 'background_work_changed') continue;
			for (const task of (ev.payload as { tasks: BackgroundTask[] }).tasks) background.set(task.task_id, task);
		}
		const todos = new Map<string | null, TodoList>();
		for (const ev of this.hub.log(`conversation:${id}`)) {
			if (ev.kind !== 'todo_updated') continue;
			const list = ev.payload as TodoList;
			todos.set(list.lane_block_id ?? null, list);
		}
		const held = this.details.get(id)?.queue as Partial<Record<keyof QueueSnapshot, unknown>> | undefined;

		return {
			detail: this.details.get(id) ?? { id, title: 'Сессия' },
			queue: {
				pause_reasons: (held?.pause_reasons ?? []) as QueueSnapshot['pause_reasons'],
				hold_reasons: (held?.hold_reasons ?? []) as QueueSnapshot['hold_reasons'],
				run_slot: (held?.run_slot ?? 'free') as QueueSnapshot['run_slot'],
				queued: this.queuedInputs(id)
			},
			history: this.page(id, {}),
			scheduled: [],
			open_requests,
			todos: [...todos.values()].sort((a, b) => Number(a.lane_block_id !== null) - Number(b.lane_block_id !== null)),
			background: [...background.values()]
		};
	}

	/** Как `todo_updated` хаба: список целиком, ревизия дорожки растёт с каждым обновлением. */
	pushTodo(id: string, items: TodoList['items'], lane: string | null = null, omitted = 0): HubEvent {
		const known = this.hub
			.log(`conversation:${id}`)
			.filter((ev) => ev.kind === 'todo_updated' && (ev.payload as TodoList).lane_block_id === lane).length;
		return this.hub.append(`conversation:${id}`, {
			kind: 'todo_updated',
			conversation_id: id,
			payload: { lane_block_id: lane, revision: known + 1, items, omitted }
		});
	}

	/** Как `background_work_changed` хаба: только изменившиеся задачи. */
	pushBackground(id: string, tasks: BackgroundTask[]): HubEvent {
		return this.hub.append(`conversation:${id}`, {
			kind: 'background_work_changed',
			conversation_id: id,
			payload: { tasks }
		});
	}

	/** Как `queued` снимка хаба: вводы без места в ленте целиком, а не их число. */
	queuedInputs(id: string): QueueSnapshot['queued'] {
		const held = new Map<string, QueueSnapshot['queued'][number]>();
		for (const item of this.histories.get(id) ?? []) {
			if (waiting(item)) held.set(item.id, { ...item, feed_seq: null } as QueueSnapshot['queued'][number]);
		}
		for (const ev of this.hub.log(`conversation:${id}`)) {
			const p = ev.payload as Record<string, unknown>;
			if (ev.kind === 'input_queued') {
				held.set(String(p.input_id), {
					id: String(p.input_id),
					idempotency_key: String(p.idempotency_key),
					state: String(p.state ?? 'queued'),
					content: (p.content ?? []) as QueueSnapshot['queued'][number]['content'],
					attachments: [],
					created_seq: String(p.input_seq ?? ev.scope_seq),
					feed_seq: null,
					created_at: ev.committed_at,
					retry_of: null
				});
			} else if (ev.kind === 'input_state_changed') {
				const view = held.get(String(p.input_id));
				if (!view) continue;
				if (WAITING.has(String(p.state))) view.state = String(p.state);
				else held.delete(String(p.input_id));
			}
		}
		return [...held.values()];
	}

	listData(): ListSnapshot {
		return {
			counts: this.counts,
			groups: this.groups,
			items: [...this.list.values()],
			limits: this.limits,
			truncated: this.truncated,
			presets: this.presetList()
		};
	}

	presetSummary(preset: FakePreset): PresetSummary {
		return {
			id: preset.id,
			kind: preset.kind,
			title: preset.title,
			description: preset.description,
			revision: preset.revision,
			text_bytes: preset.text_bytes,
			created: preset.created,
			last_edit: preset.last_edit,
			used_by: preset.used_by
		};
	}

	presetList(): PresetSummary[] {
		const rank = (kind: string): number => kind === 'clean' ? 0 : kind === 'owner' ? 1 : 2;
		return [...this.presets.values()]
			.sort((a, b) => rank(a.kind) - rank(b.kind) || a.title.localeCompare(b.title, 'ru'))
			.map((preset) => this.presetSummary(preset));
	}

	bumpPresetRevision(id: string, text: string): void {
		const preset = this.presets.get(id);
		if (!preset || preset.revision == null) return;
		preset.revision += 1;
		preset.text = text;
		preset.text_bytes = new TextEncoder().encode(text).length;
		this.hub.append('conversations', { kind: 'instruction_preset_changed', payload: { preset: this.presetSummary(preset), deleted: false } });
		this.hub.flush();
	}

	applyPresetDeletion(id: string): boolean {
		const preset = this.presets.get(id);
		if (!preset) return false;
		this.presets.delete(id);
		this.deletedPresets.set(id, preset);
		this.hub.append('conversations', { kind: 'instruction_preset_changed', payload: { preset: this.presetSummary(preset), deleted: true } });
		for (const [conversationId, detail] of this.details) {
			const settings = (detail as Record<string, unknown>).settings as {
				effective?: unknown;
				pending?: { instruction_profile?: string | null };
				rev?: number;
			} | undefined;
			if (settings?.pending?.instruction_profile === id) {
				const pending = { ...(settings.pending as Record<string, unknown>), instruction_profile: null };
				(detail as Record<string, unknown>).settings = { ...settings, pending };
				this.hub.append(`conversation:${conversationId}`, {
					kind: 'conversation_settings_changed',
					conversation_id: conversationId,
					payload: { effective: settings.effective, pending, rev: (settings.rev ?? 0) + 1, rejected: { keys: ['instruction_profile'], reason: 'preset_deleted' } }
				});
			}
		}
		this.hub.flush();
		return true;
	}

	async conversationSnapshot(id: string): Promise<{ cursor: string; data: SnapshotData }> {
		const snap = this.hub.snapshot(`conversation:${id}`);
		return { cursor: snap.cursor, data: (snap.data as SnapshotData | null) ?? this.conversationData(id) };
	}

	async history(id: string, query: { before?: string; after?: string; limit?: number } = {}): Promise<HistoryPage> {
		return this.page(id, query);
	}

	page(id: string, query: { before?: string; after?: string; limit?: number }): HistoryPage {
		const placed = [...this.requestRows(id).values()].flatMap((row): HistoryItem[] =>
			row.created_seq
				? [
						{
							kind: 'request',
							id: String(row.id),
							request_kind: String(row.kind),
							state: String(row.state),
							request_payload: row.request_payload,
							decision_payload: row.decision_payload,
							created_seq: String(row.created_seq),
							created_at: String(row.created_at)
						}
					]
				: []
		);
		const seeded = this.histories.get(id) ?? [];
		const known = new Set(seeded.map((item) => item.id));
		const all = [...seeded, ...placed.filter((item) => !known.has(item.id))]
			.filter((item) => !waiting(item))
			.sort((a, b) => cmp(seqOf(a), seqOf(b)));
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

	/** Те же правила, что у воркера: только каталоги родителя, скрытые по префиксу с точкой. */
	suggestWorkdirs(path: string): { workdirs: string[] } {
		const cut = path.lastIndexOf('/') + 1;
		const [parent, prefix] = [path.slice(0, cut), path.slice(cut)];
		const workdirs = this.directories.filter((dir) => {
			if (!dir.startsWith(path)) return false;
			const name = dir.slice(parent.length);
			return !name.includes('/') && (prefix.startsWith('.') || !name.startsWith('.'));
		});
		return { workdirs: workdirs.sort().slice(0, 30) };
	}

	async machinesSnapshot(): Promise<{ cursor: string; data: { items: MachineRecord[] } }> {
		const snap = this.hub.snapshot('machines');
		return { cursor: snap.cursor, data: (snap.data as { items: MachineRecord[] } | null) ?? { items: [...this.machines.values()] } };
	}

	/** Как хаб: потомки корня любой глубины страницами от нового к старому, корень отдельно. */
	async lane(id: string, blockId: string, query: { before?: string; limit?: number } = {}): Promise<LanePage> {
		const blocks = (this.histories.get(id) ?? []).filter((item): item is HistoryBlock => item.kind === 'block');
		const root = blocks.find((block) => block.id === blockId);
		if (!root) throw new Error('not_found');
		const keep = new Set<string>([blockId]);
		for (let grew = true; grew; ) {
			grew = false;
			for (const block of blocks) {
				if (block.parent_block_id && keep.has(block.parent_block_id) && !keep.has(block.id)) {
					keep.add(block.id);
					grew = true;
				}
			}
		}
		const below = blocks
			.filter((block) => block.id !== blockId && keep.has(block.id) && (!query.before || cmp(block.created_seq, query.before) < 0))
			.sort((a, b) => cmp(a.created_seq, b.created_seq));
		const page = below.slice(-(query.limit ?? 100));
		const strip = (block: HistoryBlock): Omit<HistoryBlock, 'kind'> => {
			const rest: Partial<HistoryBlock> = { ...block };
			delete rest.kind;
			return rest as Omit<HistoryBlock, 'kind'>;
		};
		return {
			root: strip(root),
			items: page.map(strip),
			next_before: below.length > page.length ? (page[0]?.created_seq ?? null) : null
		};
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

type BackgroundTask = components['schemas']['BackgroundTaskView'];

function seqOf(item: HistoryItem): string {
	return item.kind === 'input' ? item.feed_seq || item.created_seq : item.created_seq;
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
