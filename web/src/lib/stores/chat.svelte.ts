import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import type { OpJournal } from '$lib/ops/journal';
import type { DraftStore } from '$lib/drafts/store';
import type { ConnectionManager } from '$lib/sync/connection';
import type { AppliedEvent, RangeResult, ScopeApplier, ScopeStatus } from '$lib/sync/scope';
import type { HubEvent, ScopeName } from '$lib/sync/wire';
import {
	BlockModel,
	BlockSync,
	applyBlockEvent,
	type BlockIO,
	type BlockKind,
	type BlockPage
} from './blocks';
import { utf8Length } from './blocks';
import {
	assignFeedSeq,
	freshInput,
	marksFor,
	placeInput,
	type AttachmentRef,
	type InputUiState,
	type InputView
} from './inputs';
import { TimelineModel, cmpSeq, type TimelineEntry } from './timeline';

export interface HistoryBlock {
	kind: 'block';
	id: string;
	block_kind: BlockKind;
	content: string;
	content_bytes: number;
	content_truncated: boolean;
	created_seq: string;
	created_at: string;
	rev: number;
	state: string;
	parent_block_id?: string | null;
	agent?: string;
	model?: string;
	tool_name?: string | null;
	summary?: string | null;
	input_command_id?: string | null;
	incomplete_reason?: string | null;
	line_count?: number | null;
}

export interface HistoryInput {
	kind: 'input';
	id: string;
	idempotency_key: string;
	state: string;
	content: { type: string; text?: string; attachment_id?: string; name?: string; mime?: string; size?: number; sha256?: string; image?: boolean }[];
	attachments: AttachmentRef[];
	created_seq: string;
	feed_seq?: string | null;
	created_at: string;
	retry_of?: string | null;
}

export type HistoryItem = HistoryBlock | HistoryInput;

export interface HistoryPage {
	items: HistoryItem[];
	next_before?: string | null;
	next_after?: string | null;
}

export interface ChatApi {
	conversationSnapshot(id: string): Promise<{ cursor: string; data: SnapshotData }>;
	history(id: string, query?: { before?: string; after?: string; limit?: number }): Promise<HistoryPage>;
	events(scope: string, after: string, limit: number): Promise<RangeResult>;
	markRead(id: string, seq: string): Promise<void>;
}

export interface SnapshotData {
	detail?: Record<string, unknown> | null;
	queue?: { pause_reasons?: unknown[]; hold_reasons?: unknown[]; queued?: number; run_slot?: RunSlot };
	history?: HistoryPage;
	scheduled?: unknown[];
}

export type RunSlot =
	| 'free'
	| { dispatching: { input_id: string; attempt: number } }
	| { running: { input_id: string } };

type Clock = { now(): number; schedule?: (fn: () => void, ms: number) => () => void };

const SERVER_STATES = new Set<InputUiState>([
	'queued',
	'dispatched',
	'on_machine',
	'transmitting',
	'accepted',
	'acceptance_unconfirmed',
	'cancelled',
	'rejected'
]);

function asState(value: string): InputUiState {
	return SERVER_STATES.has(value as InputUiState) ? (value as InputUiState) : 'queued';
}

function textOf(content: HistoryInput['content']): string {
	return content
		.filter((block) => block.type === 'text')
		.map((block) => block.text ?? '')
		.join('');
}

function scopeOf(id: string): ScopeName {
	return `conversation:${id}`;
}

export class ChatStore {
	readonly blocks = new SvelteMap<string, BlockModel>();
	readonly inputs = new SvelteMap<string, InputView>();
	readonly timeline = new TimelineModel();
	readonly requests: HubEvent[] = [];
	readonly diagnostics: unknown[] = [];
	readonly queue = $state({
		pause_reasons: [] as unknown[],
		hold_reasons: [] as unknown[],
		run_slot: 'free' as RunSlot,
		queued: 0
	});
	detail = $state<Record<string, unknown> | null>(null);
	execution = $state<Record<string, unknown> | null>(null);
	activity = $state<Record<string, unknown> | null>(null);
	settings = $state<{ effective: unknown; pending: unknown; rev: number }>({ effective: null, pending: null, rev: 0 });
	crunch = $state<{ enabled: boolean; resume_pending: boolean; resume_text?: string | null; reserve_override_until?: string | null }>({
		enabled: false,
		resume_pending: false
	});
	scheduled = $state<unknown[]>([]);
	phase = $state<Record<string, unknown> | null>(null);
	interrupt = $state<Record<string, unknown> | null>(null);
	attachments = new SvelteMap<string, Record<string, unknown>>();
	unknownKinds = $state(0);
	hiddenCount = $state(0);
	queuePausedCount = $state(0);
	following = true;

	readonly #sync: BlockSync;
	#sub: { release(): void; readonly status: ScopeStatus; readonly cursor: string } | null = null;
	#hold = false;
	#held: HubEvent[] = [];
	#readSeq = 0n;
	#readAt = -10_000;
	#noteTimers = new SvelteMap<string, () => void>();

	constructor(
		readonly id: string,
		private deps: {
			api: ChatApi;
			ops: OpJournal;
			drafts: DraftStore;
			connection: ConnectionManager;
			blockIO: BlockIO;
			clock: Clock;
			probe: unknown;
		}
	) {
		this.#sync = new BlockSync(this.blocks, deps.blockIO);
	}

	get status(): ScopeStatus {
		return this.#sub?.status ?? 'idle';
	}

	get applier(): ScopeApplier {
		return {
			loadSnapshot: async () => {
				const snap = await this.deps.api.conversationSnapshot(this.id);
				return { cursor: snap.cursor, data: snap.data };
			},
			applySnapshot: (data) => {
				this.#applySnapshot(data as SnapshotData);
			},
			applyEvents: (batch) => this.#applyBatch(batch),
			fetchRange: (after, limit) => this.deps.api.events(scopeOf(this.id), after, limit)
		};
	}

	open(): void {
		this.#adoptSaving();
		if (this.#sub) return;
		this.#sub = this.deps.connection.subscribe(scopeOf(this.id), this.applier);
	}

	close(): void {
		this.#sub?.release();
		this.#sub = null;
		for (const cancel of this.#noteTimers.values()) cancel();
		this.#noteTimers.clear();
	}

	async loadOlder(): Promise<void> {
		const page = await this.deps.api.history(this.id, { before: this.timeline.oldestSeq() ?? undefined });
		this.#ingest(page.items, 'older');
		this.timeline.hasOlder = Boolean(page.next_before);
	}

	async loadNewer(): Promise<void> {
		this.#hold = true;
		try {
			const page = await this.deps.api.history(this.id, { after: this.timeline.newestSeq() ?? undefined });
			this.#ingest(page.items, 'newer');
			this.timeline.tailDetached = false;
			this.timeline.newerCount = 0;
		} finally {
			this.#hold = false;
			this.#flushHeld();
		}
	}

	async reloadTail(): Promise<void> {
		this.#hold = true;
		try {
			const page = await this.deps.api.history(this.id, {});
			this.#ingest(page.items, 'tail');
		} finally {
			this.#hold = false;
			this.#flushHeld();
		}
	}

	markRead(createdSeq: string): void {
		if (!this.following || !visible()) return;
		let seq: bigint;
		try {
			seq = BigInt(createdSeq);
		} catch {
			return;
		}
		if (seq <= this.#readSeq) return;
		const now = this.deps.clock.now();
		if (now - this.#readAt < 2000) return;
		this.#readSeq = seq;
		this.#readAt = now;
		void this.deps.api.markRead(this.id, createdSeq);
	}

	async blockContent(blockId: string): Promise<AsyncIterable<BlockPage>> {
		const io = this.deps.blockIO;
		return (async function* () {
			let offset = 0;
			for (;;) {
				const page = await io.fetchContent(blockId, offset);
				yield page;
				const next = page.next_offset ?? page.offset + utf8Length(page.content);
				if (next <= offset || next >= page.total_bytes) break;
				offset = next;
			}
		})();
	}

	strip(): InputView[] {
		this.#adoptSaving();
		return [...this.inputs.values()].filter((input) => input.placement === 'strip' || (input.state === 'cancelled' && input.stripUntil !== null));
	}

	#applyBatch(batch: AppliedEvent[]): void {
		for (const item of batch) this.#take(item.event);
	}

	#take(event: HubEvent): void {
		if (event.kind === 'hidden') {
			this.hiddenCount += 1;
			return;
		}
		if (this.#hold || this.timeline.tailDetached) {
			this.#held.push(event);
			if (this.timeline.tailDetached && (event.kind === 'block_started' || event.kind === 'input_queued')) {
				this.timeline.newerCount += 1;
			}
			return;
		}
		this.#dispatch(event);
	}

	#flushHeld(): void {
		const events = this.#held.splice(0);
		for (const event of events) this.#dispatch(event);
	}

	#dispatch(event: HubEvent): void {
		const kind = event.kind as string;
		switch (kind) {
			case 'hidden':
				this.hiddenCount += 1;
				return;
			case 'input_queued':
				this.#queued(event);
				return;
			case 'input_state_changed':
				this.#inputState(event);
				return;
			case 'block_started':
			case 'block_delta':
			case 'block_final':
			case 'block_incomplete':
				this.#block(event);
				return;
			case 'run_finished':
				if (event.kind === 'run_finished') this.execution = { ...(this.execution ?? {}), last_run: event.payload };
				return;
			case 'phase_changed':
				if (event.kind === 'phase_changed') this.phase = { ...event.payload };
				return;
			case 'queue_state_changed':
				if (event.kind === 'queue_state_changed') {
					this.queue.pause_reasons = event.payload.pause_reasons;
					this.queue.hold_reasons = event.payload.hold_reasons;
					this.queue.run_slot = event.payload.run_slot;
				}
				return;
			case 'queue_paused':
				this.queuePausedCount += 1;
				return;
			case 'queue_resumed':
				if (event.kind === 'queue_resumed') this.queue.pause_reasons = event.payload.pause_reasons;
				return;
			case 'execution_state_changed':
				if (event.kind === 'execution_state_changed') this.execution = { ...event.payload };
				return;
			case 'conversation_settings_changed':
				if (event.kind === 'conversation_settings_changed') {
					this.settings = { effective: event.payload.effective, pending: event.payload.pending, rev: event.payload.rev };
				}
				return;
			case 'conversation_updated':
				if (event.kind === 'conversation_updated') this.detail = { ...(this.detail ?? {}), ...event.payload };
				return;
			case 'crunch_changed':
				if (event.kind === 'crunch_changed') {
					this.crunch = {
						enabled: event.payload.enabled,
						resume_text: event.payload.resume_text,
						reserve_override_until: event.payload.reserve_override_until,
						resume_pending: event.payload.resume_pending
					};
				}
				return;
			case 'crunch_held':
				if (event.kind === 'crunch_held') this.crunch = { ...this.crunch, resume_pending: event.payload.resume_pending };
				return;
			case 'crunch_woken':
				if (event.kind === 'crunch_woken') this.crunch = { ...this.crunch, resume_pending: event.payload.resume_pending };
				return;
			case 'limit_reached':
				if (event.kind === 'limit_reached') this.diagnostics.push(event);
				return;
			case 'scheduled_changed':
				if (event.kind === 'scheduled_changed') {
					const next = this.scheduled.filter((item) => (item as { id?: string }).id !== event.payload.id);
					next.push(event.payload);
					this.scheduled = next;
				}
				return;
			case 'attachment_state_changed':
				if (event.kind === 'attachment_state_changed') {
					const prev = this.attachments.get(event.payload.attachment_id) ?? {};
					const next: Record<string, unknown> = { ...prev, ...(event.payload as unknown as Record<string, unknown>) };
					this.attachments.set(event.payload.attachment_id, next);
					for (const input of this.inputs.values()) {
						for (let i = 0; i < input.attachments.length; i++) {
							if (input.attachments[i]?.attachment_id === event.payload.attachment_id) {
								const current = input.attachments[i]!;
								input.attachments[i] = {
									attachment_id: event.payload.attachment_id,
									name: typeof next.name === 'string' ? next.name : current.name,
									mime: typeof next.mime === 'string' ? next.mime : current.mime,
									size: typeof next.size === 'number' ? next.size : current.size,
									sha256: typeof next.sha256 === 'string' ? next.sha256 : current.sha256,
									image: typeof next.image === 'boolean' ? next.image : current.image
								};
							}
						}
					}
				}
				return;
			case 'derived_changed':
				if (event.kind === 'derived_changed') {
					this.activity = { ...event.payload.activity };
					this.queue.queued = event.payload.queue.queued;
				}
				return;
			case 'interrupt_state_changed':
				if (event.kind === 'interrupt_state_changed') this.interrupt = { ...event.payload };
				return;
			case 'request_opened':
			case 'request_closed':
			case 'request_decided':
				this.requests.push(event);
				return;
			case 'block_conflicting_final':
			case 'adapter_diagnostic':
			case 'state_divergence':
			case 'block_revision_gap':
			case 'block_checksum_mismatch':
				this.diagnostics.push(event);
				return;
			default:
				this.unknownKinds += 1;
		}
	}

	#block(event: HubEvent): void {
		const effects = applyBlockEvent(this.blocks, event);
		this.#sync.handle(effects);
		for (const effect of effects) if (effect.type === 'diag') this.diagnostics.push(effect);
		if (event.kind === 'block_started') {
			const block = this.blocks.get(event.payload.block_id);
			if (block) this.timeline.upsert({ kind: 'block', key: `block:${block.id}`, seq: block.createdSeq, block });
		}
	}

	#queued(event: HubEvent): void {
		if (event.kind !== 'input_queued') return;
		const payload = event.payload;
		const existing = this.inputs.get(payload.idempotency_key) ?? freshInput(payload.idempotency_key, '', this.deps.clock.now());
		existing.id = payload.input_id;
		existing.text = textOf(payload.content as HistoryInput['content']);
		const rawContent = (payload.content as Array<{ type: string; attachment_id?: string }> | undefined) ?? [];
		const attBlocks = rawContent.filter((b) => b.type === 'attachment' && typeof b.attachment_id === 'string');
		if (attBlocks.length > 0) {
			existing.attachments = attBlocks.map((b) => {
				const registered = this.attachments.get(b.attachment_id!);
				if (registered) {
					return {
						attachment_id: String(registered.attachment_id ?? b.attachment_id),
						name: typeof registered.name === 'string' ? registered.name : '',
						mime: typeof registered.mime === 'string' ? registered.mime : '',
						size: typeof registered.size === 'number' ? registered.size : 0,
						sha256: typeof registered.sha256 === 'string' ? registered.sha256 : '',
						image: Boolean(registered.image)
					};
				}
				const prior = existing.attachments.find((a) => a.attachment_id === b.attachment_id);
				if (prior) return prior;
				return {
					attachment_id: b.attachment_id!,
					name: '',
					mime: '',
					size: 0,
					sha256: '',
					image: false
				};
			});
		}
		existing.state = 'queued';
		existing.placement = placeInput('queued');
		existing.marks = marksFor('queued');
		existing.localOnly = false;
		existing.createdSeq = payload.input_seq;
		this.inputs.set(existing.key, existing);
		this.timeline.remove(`input:${existing.key}`);
		this.deps.ops.settle(existing.key);
	}

	#inputState(event: HubEvent): void {
		if (event.kind !== 'input_state_changed') return;
		const payload = event.payload;
		const input = [...this.inputs.values()].find((item) => item.id === payload.input_id);
		if (!input) return;
		const state = asState(payload.state);
		input.state = state;
		input.marks = marksFor(state);
		input.attempt = payload.attempt ?? input.attempt;
		if (payload.retry_of) input.retryOf = payload.retry_of;
		if (payload.reason) input.rejection = payload.reason;
		if (payload.cause === 'cancel_requested') input.cancelRequested = true;
		assignFeedSeq(input, state, event.scope_seq);
		if (state === 'cancelled') {
			input.placement = 'strip';
			input.stripUntil = this.deps.clock.now() + 4000;
			this.timeline.remove(`input:${input.key}`);
			this.#armNote(input.key);
			return;
		}
		input.placement = placeInput(state);
		input.stripUntil = null;
		if (input.placement === 'feed' && input.feedSeq) {
			this.timeline.upsert({ kind: 'input', key: `input:${input.key}`, seq: input.feedSeq, input });
		} else this.timeline.remove(`input:${input.key}`);
	}

	#armNote(key: string): void {
		this.#noteTimers.get(key)?.();
		const drop = (): void => {
			this.#noteTimers.delete(key);
			const input = this.inputs.get(key);
			if (!input || input.state !== 'cancelled') return;
			input.stripUntil = null;
			input.placement = 'none';
			this.inputs.delete(key);
		};
		if (this.deps.clock.schedule) this.#noteTimers.set(key, this.deps.clock.schedule(drop, 4000));
		else {
			const timer = setTimeout(drop, 4000);
			this.#noteTimers.set(key, () => clearTimeout(timer));
		}
	}

	#applySnapshot(data: SnapshotData): void {
		if (data.detail) this.detail = data.detail;
		if (data.queue) {
			this.queue.pause_reasons = data.queue.pause_reasons ?? [];
			this.queue.hold_reasons = data.queue.hold_reasons ?? [];
			this.queue.queued = data.queue.queued ?? this.queue.queued;
			if (data.queue.run_slot) this.queue.run_slot = data.queue.run_slot;
		}
		if (data.scheduled) this.scheduled = data.scheduled;
		const items = data.history?.items ?? [];
		const oldest = [...items].sort((a, b) => cmpSeq(seqOf(a), seqOf(b)))[0];
		const floor = oldest ? seqOf(oldest) : null;
		const older = floor ? this.timeline.entries.filter((entry) => BigInt(entry.seq) < BigInt(floor)) : [];
		const keepOlder = older.every((entry) => terminalEntry(entry));
		if (!keepOlder) this.timeline.applyHistoryPage([], 'tail');
		this.#ingest(items, 'tail');
		if (!keepOlder) {
			const keptSeq = new SvelteSet(items.map((item) => (item.kind === 'block' ? `block:${item.id}` : `input:${item.idempotency_key}`)));
			for (const entry of older) if (!keptSeq.has(entry.key) && !terminalEntry(entry)) this.timeline.remove(entry.key);
		}
	}

	#ingest(items: HistoryItem[], dir: 'older' | 'newer' | 'tail'): void {
		const entries: TimelineEntry[] = [];
		for (const item of items) {
			if (item.kind === 'block') {
				const block = this.#blockFromHistory(item, dir === 'tail');
				entries.push({ kind: 'block', key: `block:${block.id}`, seq: block.createdSeq, block });
			} else {
				const input = this.#inputFromHistory(item);
				if (input.feedSeq && input.placement === 'feed') {
					entries.push({ kind: 'input', key: `input:${input.key}`, seq: input.feedSeq, input });
				}
			}
		}
		this.timeline.applyHistoryPage(entries, dir);
	}

	#blockFromHistory(item: HistoryBlock, wholesale: boolean): BlockModel {
		let block = this.blocks.get(item.id);
		if (!block) {
			block = new BlockModel(item.id, item.block_kind);
			this.blocks.set(item.id, block);
		}
		block.parentId = item.parent_block_id ?? block.parentId;
		block.agent = item.agent ?? block.agent;
		block.model = item.model ?? block.model;
		block.toolName = item.tool_name ?? block.toolName;
		block.summary = item.summary ?? block.summary;
		block.inputCommandId = item.input_command_id ?? block.inputCommandId;
		block.createdSeq = item.created_seq;
		block.createdAt = Date.parse(item.created_at);
		block.lineCount = item.line_count ?? block.lineCount;
		block.truncated = item.content_truncated;
		block.contentBytes = item.content_bytes;
		if (!block || wholesale || block.state === 'streaming' || block.state === 'stale' || block.body === '') {
			block.body = item.content;
			block.bytes = utf8Length(item.content);
			block.rev = item.rev;
			block.text = item.block_kind === 'tool_call' && item.state !== 'final' ? (item.summary ?? '') : item.content;
		}
		if (item.state === 'final' || item.state === 'incomplete' || item.state === 'streaming' || item.state === 'stale') {
			block.state = item.state;
		}
		block.incompleteReason = item.incomplete_reason ?? block.incompleteReason;
		return block;
	}

	#inputFromHistory(item: HistoryInput): InputView {
		const state = asState(item.state);
		const input = this.inputs.get(item.idempotency_key) ?? freshInput(item.idempotency_key, textOf(item.content), Date.parse(item.created_at));
		input.id = item.id;
		input.text = textOf(item.content);
		input.attachments = item.attachments;
		input.state = state;
		input.placement = placeInput(state);
		input.marks = marksFor(state);
		input.localOnly = false;
		input.createdSeq = item.created_seq;
		input.createdAt = Date.parse(item.created_at);
		input.retryOf = item.retry_of ?? null;
		if (item.feed_seq) input.feedSeq = item.feed_seq;
		this.inputs.set(input.key, input);
		this.deps.ops.settle(input.key);
		return input;
	}

	#adoptSaving(): void {
		for (const op of this.deps.ops.ops.values()) {
			if (op.kind !== 'input' || op.conversationId !== this.id) continue;
			if (op.state !== 'saving' && op.state !== 'unknown') continue;
			if (this.inputs.has(op.key)) continue;
			const text = typeof (op.body as { text?: string } | null)?.text === 'string' ? (op.body as { text: string }).text : '';
			const attachments = Array.isArray((op.body as { attachments?: AttachmentRef[] } | null)?.attachments)
				? (op.body as { attachments: AttachmentRef[] }).attachments
				: [];
			this.inputs.set(op.key, freshInput(op.key, text, op.createdAt, attachments));
		}
	}
}

function visible(): boolean {
	return typeof document === 'undefined' || document.visibilityState !== 'hidden';
}

function seqOf(item: HistoryItem): string {
	if (item.kind === 'block') return item.created_seq;
	return item.feed_seq || item.created_seq;
}

function terminalEntry(entry: TimelineEntry): boolean {
	if (entry.kind === 'block') return entry.block.state === 'final' || entry.block.state === 'incomplete';
	return entry.input.state === 'accepted' || entry.input.state === 'rejected' || entry.input.state === 'cancelled' || entry.input.state === 'acceptance_unconfirmed';
}
