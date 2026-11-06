import { utf8Length, utf8Slice, type BlockPage } from '../../src/lib/stores/blocks';
import type { AlarmView, HistoryBlock, HistoryInput, HistoryItem, HistoryPage, LanePage, SnapshotData } from '../../src/lib/stores/chat.svelte';
import type { DelegationNode } from '../../src/lib/stores/delegation';
import type { Counts, ListRow, ListSnapshot } from '../../src/lib/stores/list.svelte';
import type { ProviderLimits } from '../../src/lib/stores/limits';
import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import type { RunInfo } from '../../src/lib/stores/run';
import type { TodoList } from '../../src/lib/stores/todos.svelte';
import type { RangeResult } from '../../src/lib/sync/scope';
import type { HubEvent } from '../../src/lib/sync/wire';
import type { DraftRecord } from '../../src/lib/drafts/sync';
import type { FakeHub } from './hub';
import type { components } from '../../src/lib/api/generated/openapi.js';

type PresetSummary = components['schemas']['PresetSummary'];
type QueueSnapshot = components['schemas']['SnapshotQueueView'];
export type NudgeRuleView = components['schemas']['NudgeRuleView'];
export type NudgeEventView = components['schemas']['NudgeEventView'];
type QueueItemView = components['schemas']['QueueItemView'];

export const NUDGE_BUILTIN_TEXT =
	'[canterlot] Automatic reminder from canterlot, not the owner: you ended your turn without a final status. If the task is finished, give your report and end it with the line {markers}. If you are blocked, explain why and end with STATUS: BLOCKED. Otherwise continue from where you stopped. Do not end your turn while anything is still running or uncommitted.\nObserved: {signals}.';

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
	kind: 'clean' | 'owner' | 'custom' | 'nudge';
	purpose: 'instructions' | 'nudge';
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
	readonly usage = new Map<string, components['schemas']['SessionUsageView']>();
	readonly machines = new Map<string, MachineRecord>();
	directories = ['/home/owner', '/home/owner/projects', '/srv/canterlot', '/srv/canterlot/web', '/srv/canterlot/.git', '/srv/notes'];
	readonly historyCalls: { machine: string; workdir: string }[] = [];
	claudeHistoryItems: components['schemas']['ClaudeHistoryView'][] = [
		{ id: '01a1a7af-6e9b-7517-8b6e-92d714b66fc3', modified_at_ms: Date.UTC(2026, 10, 4, 18, 30), size: 184_320, preview: 'Почини удалённую сессию: нужен список её истории' },
		{ id: '01a1b001-0000-7000-8000-000000000002', modified_at_ms: Date.UTC(2026, 10, 1, 9, 5), size: 2048, preview: 'Короткая беседа' }
	];
	readonly reads: { id: string; seq: string }[] = [];
	readonly decisions = new Map<string, { winnerId: string; decision: unknown; key: string }>();
	readonly deleted = new Set<string>();
	readonly drafts = new Map<string, DraftRecord>();
	readonly attachments = new Map<string, { size: number; offset: number; key: string; name: string; mime: string; conversation_id: string }>();
	draftRevision = 0;
	groups: ListSnapshot['groups'] = [];
	counts: Counts = { all: 0, pinned: 0, archived: 0, trashed: 0, groups: [] };
	limits: ProviderLimits[] = [];
	truncated = false;
	presets = new Map<string, FakePreset>();
	readonly nudgeRules = new Map<string, NudgeRuleView>();
	readonly nudgeEvents = new Map<string, NudgeEventView[]>();
	/** Очередь запусков разговора: ожидающие и недавно завершённые элементы. */
	readonly spawnQueue = new Map<string, QueueItemView[]>();
	ownerOverview = {
		files: [
			{ kind: 'claude_user', harness: 'claude', path: '/home/owner/.claude/CLAUDE.md', present: true, size: 24, mtime: '2026-10-28T12:00:00.000Z', reason: '' },
			{ kind: 'claude_home', harness: 'claude', path: '/home/owner/CLAUDE.md', present: false, size: 0, mtime: null, reason: 'symlink' },
			{ kind: 'codex_global', harness: 'codex', path: '/home/owner/.codex/AGENTS.md', present: true, size: 18, mtime: '2026-10-28T12:00:00.000Z', reason: '' },
			{ kind: 'codex_override', harness: 'codex', path: '/home/owner/.codex/AGENTS.override.md', present: false, size: 0, mtime: null, reason: '' },
			{ kind: 'opencode_global', harness: 'opencode', path: '/home/owner/.config/opencode/AGENTS.md', present: true, size: 20, mtime: '2026-10-28T12:00:00.000Z', reason: '' }
		],
		memory_projects: [{ id: 'proj1', files: 2, has_index: true }]
	};
	ownerFiles = new Map<string, { kind: string; harness: string; path: string; present: boolean; size: number; mtime: string | null; truncated: boolean; content: string; files: { name: string; size: number; mtime: string | null }[] }>([
		['claude_user', { kind: 'claude_user', harness: 'claude', path: '/home/owner/.claude/CLAUDE.md', present: true, size: 24, mtime: '2026-10-28T12:00:00.000Z', truncated: false, content: '# Пользователь\n\nСтиль: кратко.', files: [] }],
		['codex_global', { kind: 'codex_global', harness: 'codex', path: '/home/owner/.codex/AGENTS.md', present: true, size: 18, mtime: '2026-10-28T12:00:00.000Z', truncated: false, content: '# Codex\n\nТесты перед коммитом.', files: [] }],
		['opencode_global', { kind: 'opencode_global', harness: 'opencode', path: '/home/owner/.config/opencode/AGENTS.md', present: true, size: 20, mtime: '2026-10-28T12:00:00.000Z', truncated: false, content: '# OpenCode\n\nПроверяй сборку.', files: [] }],
		['memory_index:proj1', { kind: 'memory_index', harness: 'claude', path: '/home/owner/.claude/projects/proj1/memory/MEMORY.md', present: true, size: 16, mtime: '2026-10-28T12:00:00.000Z', truncated: false, content: '- тема в a.md', files: [{ name: 'MEMORY.md', size: 16, mtime: '2026-10-28T12:00:00.000Z' }, { name: 'a.md', size: 5, mtime: '2026-10-28T12:00:00.000Z' }] }],
		['memory_file:proj1:a.md', { kind: 'memory_file', harness: 'claude', path: '/home/owner/.claude/projects/proj1/memory/a.md', present: true, size: 5, mtime: '2026-10-28T12:00:00.000Z', truncated: false, content: '# Тема', files: [] }]
	]);
	/** `null`: встроенный список по умолчанию, как у хаба. */
	spawnWhitelist: { harness: string; model: string }[] | null = null;
	/** `null`: встроенная политика по умолчанию, как у хаба. */
	routingPolicy: unknown | null = null;
	routingPolicyRevision = 0;
	readonly builtinRoutingPolicy = {
		task_types: {
			bugfix: {
				title: 'Исправление по ясному брифу',
				candidates: [
					{ harness: 'opencode', model: 'opencode-go/deepseek-v4.1-flash', effort: 'high', nudge: { max_nudges: 2 }, watchdog_minutes: 90, exclude_workdirs: [] },
					{ harness: 'claude_code', model: 'opus[1m]', effort: 'high', nudge: false, exclude_workdirs: [] }
				],
				isolated: true
			},
			review: {
				title: 'Взгляд со стороны',
				candidates: [{ harness: 'codex', model: 'gpt-6.1-sol', effort: 'high', nudge: false, exclude_workdirs: [] }],
				isolated: false
			}
		},
		providers: { claude: { max_weekly_percent: 90 }, codex: { max_weekly_percent: 80 }, opencode: { max_weekly_percent: 95 } },
		unlimited_models: ['opencode-go/beam-free'],
		max_parallel: { 'codex/gpt-6.1-sol': 1 },
		unknown_limits: 'allow'
	};
	readonly spawnWhitelistKeys: string[] = [];
	/** `null`: пороги лимитов по умолчанию, как у хаба. */
	limitSettings: components['schemas']['LimitSettings'] | null = null;
	readonly limitSettingsKeys: string[] = [];
	readonly defaultLimitSettings: components['schemas']['LimitSettings'] = {
		warn_remaining_pct: 20,
		danger_remaining_pct: 5,
		reserve_remaining_pct: 5
	};
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

	routingPolicyView(): { policy: unknown | null; revision: number; builtin: unknown } {
		return { policy: this.routingPolicy, revision: this.routingPolicyRevision, builtin: this.builtinRoutingPolicy };
	}

	/** Сохранение политики: `false` — устаревшая ревизия, как `409` у хаба. */
	saveRoutingPolicy(policy: unknown | null, revision: number): boolean {
		if (revision !== this.routingPolicyRevision) return false;
		this.routingPolicy = policy;
		this.routingPolicyRevision += 1;
		this.hub.append('conversations', { kind: 'routing_policy_changed', payload: { revision: this.routingPolicyRevision } });
		return true;
	}

	/** Причины пропуска для предпросмотра: ключ `harness/model`, значение — вердикт и английская причина хаба. */
	readonly routingSkips = new Map<string, { verdict: string; reason: string }>();
	/** Вердикт стража в предпросмотре. */
	routingGuard: { stale: boolean; blocked: boolean; violations: string[]; detail: string | null } = { stale: false, blocked: false, violations: [], detail: null };
	/** Отказ `no_executor_available`: разбор по кандидатам и ближайший сброс в `details`, как у хаба. */
	routingRefusal: { candidates: { harness: string; model: string; verdict: string; reason: string }[]; nearest_reset: string | null } | null = null;
	readonly routingPreviews: { task_type: string; level?: string; workdir?: string }[] = [];

	/** Предпросмотр выбора: кандидаты по порядку, первый не пропущенный выбран, как у хаба. */
	previewRoutingPolicy(task_type: string, level?: string, workdir?: string): { decision: unknown; machine: unknown; guard: unknown; considered: unknown[] } {
		this.routingPreviews.push({ task_type, level, workdir });
		const policy = (this.routingPolicy ?? this.builtinRoutingPolicy) as {
			task_types: Record<string, { title: string; candidates: { harness: string; model: string; effort?: string | null; nudge?: unknown; watchdog_minutes?: number | null }[] }>;
		};
		const task = policy.task_types[task_type];
		if (!task) {
			throw { status: 404, code: 'task_type_unknown', message: `Unknown task type ${task_type}` };
		}
		if (this.routingRefusal) throw { status: 422, code: 'no_executor_available', message: 'No executor available', details: this.routingRefusal };
		const considered: { harness: string; model: string; verdict: string; reason: string }[] = [];
		let chosen: (typeof task.candidates)[number] | undefined;
		for (const candidate of task.candidates) {
			const skip = this.routingSkips.get(`${candidate.harness}/${candidate.model}`);
			if (skip) {
				considered.push({ harness: candidate.harness, model: candidate.model, ...skip });
				continue;
			}
			considered.push({ harness: candidate.harness, model: candidate.model, verdict: 'chosen', reason: '' });
			chosen = candidate;
			break;
		}
		if (!chosen) throw { status: 422, code: 'no_executor_available', message: 'No executor available', details: { candidates: considered, nearest_reset: null } };
		const decision = {
			policy_revision: this.routingPolicyRevision,
			task_type,
			level: level ?? null,
			harness: chosen.harness,
			model: chosen.model,
			effort: chosen.effort ?? level ?? 'medium',
			preset: null,
			nudge: chosen.nudge ?? false,
			watchdog_minutes: chosen.watchdog_minutes ?? null,
			considered
		};
		return {
			decision,
			machine: { free_disk_gb: 72, load_per_core: 0.6, mem_available_gb: 41, observed_at: '2026-11-05T12:00:00.000Z', stale: false },
			guard: this.routingGuard,
			considered
		};
	}

	limitSettingsView(): components['schemas']['LimitSettingsView'] {
		return {
			settings: this.limitSettings ?? this.defaultLimitSettings,
			is_default: this.limitSettings === null,
			default_settings: this.defaultLimitSettings
		};
	}

	/** `/api/v1/config` отдаёт действующие пороги вместе с устаревшими полями. */
	limitThresholds() {
		const settings = this.limitSettings ?? this.defaultLimitSettings;
		return {
			stale_after_secs: 2100,
			warn_pcts: [80, 95],
			weekly_reserve_pct: 100 - settings.reserve_remaining_pct,
			warn_remaining_pct: settings.warn_remaining_pct,
			danger_remaining_pct: settings.danger_remaining_pct,
			reserve_remaining_pct: settings.reserve_remaining_pct
		};
	}

	/** Как настоящий хаб: окончательная уборка убирает строки и шлёт `conversation_deleted`. */
	deleteConversation(id: string): void {
		const row = this.list.get(id);
		this.list.delete(id);
		this.details.delete(id);
		this.histories.delete(id);
		this.deleted.add(id);
		this.counts = this.recount();
		if (!row) return;
		this.hub.append('conversations', {
			kind: 'conversation_deleted',
			conversation_id: id,
			payload: { id, counts: this.counts }
		});
		this.hub.flush();
	}

	/** Ручное удаление владельца: строка остаётся, но переезжает во вкладку «Корзина». */
	trashConversation(id: string): void {
		const row = this.list.get(id);
		if (!row || row.trashed_at) return;
		row.trashed_at = new Date().toISOString();
		const detail = this.details.get(id);
		if (detail) detail.trashed_at = row.trashed_at;
		this.counts = this.recount();
		this.emitUpdated(id);
	}

	restoreConversation(id: string): void {
		const row = this.list.get(id);
		if (!row) return;
		row.trashed_at = null;
		const detail = this.details.get(id);
		if (detail) detail.trashed_at = null;
		this.counts = this.recount();
		this.emitUpdated(id);
	}

	purgeConversation(id: string): void {
		this.deleteConversation(id);
	}

	recount(): Counts {
		const rows = [...this.list.values()].filter((item) => !item.trashed_at);
		return {
			all: rows.filter((item) => !item.archived).length,
			pinned: rows.filter((item) => item.pinned && !item.archived).length,
			archived: rows.filter((item) => item.archived && !item.accepted).length,
			trashed: [...this.list.values()].filter((item) => item.trashed_at).length,
			groups: this.groups.map((group) => ({
				id: group.id,
				count: rows.filter((item) => item.groups.includes(group.id) && !item.archived).length
			}))
		};
	}

	private emitUpdated(id: string): void {
		const row = this.list.get(id);
		if (!row) return;
		const payload = {
			id: row.id,
			title: row.title,
			title_source: row.title_source === 'auto' ? 'auto' : 'user',
			color: row.color,
			pinned: row.pinned,
			archived: row.archived,
			accepted: Boolean(row.accepted),
			trashed: Boolean(row.trashed_at),
			trashed_at: row.trashed_at ?? null,
			groups: row.groups,
			group_pins: row.group_pins,
			machine: row.machine,
			harness: row.harness,
			workdir: row.workdir,
			created_at: row.created_at,
			parent_id: row.parent_id,
			spawn_depth: row.spawn_depth,
			counts: this.counts
		};
		this.hub.append('conversations', { kind: 'conversation_updated', conversation_id: id, payload });
		this.hub.append(`conversation:${id}`, { kind: 'conversation_updated', conversation_id: id, payload });
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

	draft(id: string): DraftRecord {
		return this.drafts.get(id) ?? { text: '', attachment_ids: [], revision: 0, updated_at: null };
	}

	putDraft(
		id: string,
		body: { text: string; attachment_ids?: string[]; device_id?: string | null }
	): DraftRecord {
		const revision = ++this.draftRevision;
		const attachment_ids = body.attachment_ids ?? [];
		if (body.text === '' && attachment_ids.length === 0) {
			this.drafts.delete(id);
			const record: DraftRecord = { text: '', attachment_ids: [], revision, updated_at: null };
			this.hub.append(`conversation:${id}`, {
				kind: 'draft_changed',
				conversation_id: id,
				payload: {
					conversation_id: id,
					revision,
					text: '',
					attachment_ids: [],
					device_id: body.device_id ?? null
				}
			});
			this.hub.flush();
			return record;
		}
		const record: DraftRecord = {
			text: body.text,
			attachment_ids,
			revision,
			updated_at: new Date().toISOString(),
			device_id: body.device_id ?? null
		};
		this.drafts.set(id, record);
		this.hub.append(`conversation:${id}`, {
			kind: 'draft_changed',
			conversation_id: id,
			payload: {
				conversation_id: id,
				revision,
				text: record.text,
				attachment_ids: record.attachment_ids,
				device_id: record.device_id ?? null
			}
		});
		this.hub.flush();
		return record;
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
		const alarms = new Map<string, AlarmView>();
		for (const ev of this.hub.log(`conversation:${id}`)) {
			if (ev.kind === 'alarm_changed') {
				const alarm = ev.payload as AlarmView;
				if (alarm.state === 'cancelled') alarms.delete(alarm.alarm_id);
				else alarms.set(alarm.alarm_id, alarm);
				continue;
			}
			if (ev.kind !== 'todo_updated') continue;
			const list = ev.payload as TodoList;
			todos.set(list.lane_block_id ?? null, list);
		}
		const held = this.details.get(id)?.queue as Partial<Record<keyof QueueSnapshot, unknown>> | undefined;
		let context: SnapshotData['context'];
		for (const ev of this.hub.log(`conversation:${id}`)) {
			if (ev.kind !== 'context_changed') continue;
			const p = ev.payload as { tokens: unknown; window_tokens: unknown };
			context =
				typeof p.tokens === 'number'
					? { tokens: p.tokens, window_tokens: typeof p.window_tokens === 'number' ? p.window_tokens : null }
					: null;
		}

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
			draft: this.drafts.get(id) ?? null,
			alarms: [...alarms.values()],
			spawn_queue: this.spawnQueue.get(id) ?? [],
			nudge: this.nudgeRules.get(id) ?? null,
			nudge_events: this.nudgeEvents.get(id) ?? [],
			open_requests,
			todos: [...todos.values()].sort((a, b) => Number(a.lane_block_id !== null) - Number(b.lane_block_id !== null)),
			background: [...background.values()],
			context
		};
	}

	/** Элемент очереди запусков для снимка и HTTP владельца. */
	queueItem(id: string, input: Partial<QueueItemView> = {}): QueueItemView {
		const now = new Date().toISOString();
		const item: QueueItemView = {
			id: input.id ?? crypto.randomUUID(),
			parent_id: id,
			title: input.title ?? null,
			task_type: input.task_type ?? null,
			level: input.level ?? null,
			harness: input.harness ?? null,
			model: input.model ?? null,
			reason: input.reason ?? 'rate',
			detail: input.detail ?? { detail: '10 spawns in the last hour' },
			state: input.state ?? 'queued',
			not_before: input.not_before ?? now,
			expires_at: input.expires_at ?? new Date(Date.now() + 86_400_000).toISOString(),
			attempts: input.attempts ?? 0,
			child_id: input.child_id ?? null,
			result: input.result ?? null,
			created_at: input.created_at ?? now,
			updated_at: input.updated_at ?? now,
			position: input.position ?? 1
		};
		this.pushQueueItem(id, item);
		return item;
	}

	private pushQueueItem(conversation: string, item: QueueItemView): void {
		const list = this.spawnQueue.get(conversation) ?? [];
		const index = list.findIndex((known) => known.id === item.id);
		if (index >= 0) list[index] = item;
		else list.push(item);
		this.spawnQueue.set(conversation, list);
		this.hub.append(`conversation:${conversation}`, {
			kind: 'spawn_queue_changed',
			conversation_id: conversation,
			payload: item
		});
	}

	spawnQueueView(id: string): { items: QueueItemView[] } {
		return { items: this.spawnQueue.get(id) ?? [] };
	}

	cancelSpawnQueue(id: string, queueId: string): { status: string; queue_id: string; session_id: string | null } {
		const item = (this.spawnQueue.get(id) ?? []).find((known) => known.id === queueId);
		if (!item) return { status: 'already_cancelled', queue_id: queueId, session_id: null };
		if (item.state === 'started') return { status: 'already_started', queue_id: queueId, session_id: item.child_id ?? null };
		if (item.state !== 'queued') return { status: 'already_cancelled', queue_id: queueId, session_id: null };
		this.pushQueueItem(id, { ...item, state: 'cancelled', updated_at: new Date().toISOString() });
		return { status: 'cancelled', queue_id: queueId, session_id: null };
	}

	/** Причина отказа «Запустить сейчас»: хаб записывает исход (элемент снова ждёт) и отвечает `409 invalid_state`. */
	startNowRefusal: string | null = null;

	startSpawnQueueNow(id: string, queueId: string): components['schemas']['SpawnQueueActionView'] {
		const item = (this.spawnQueue.get(id) ?? []).find((known) => known.id === queueId);
		if (!item) return { status: 'already_cancelled', queue_id: queueId, session_id: null };
		if (item.state === 'queued' && this.startNowRefusal) {
			// Исход уже записан хабом: событие не публикуется, клиент перечитывает очередь сам.
			const list = this.spawnQueue.get(id) ?? [];
			list[list.indexOf(item)] = { ...item, attempts: item.attempts + 1, not_before: new Date(Date.now() + 60_000).toISOString(), updated_at: new Date().toISOString() };
			throw { status: 409, code: 'invalid_state', message: 'Элемент не запущен', details: { reason: this.startNowRefusal } };
		}
		if (item.state === 'queued') {
			const child = crypto.randomUUID();
			this.pushQueueItem(id, {
				...item,
				state: 'started',
				child_id: child,
				updated_at: new Date().toISOString()
			});
			return { status: 'started', queue_id: queueId, session_id: child, title: 'Очередной запуск', harness: 'opencode', model: 'opencode-go/beam-free' };
		}
		return { status: item.state, queue_id: queueId, session_id: item.child_id ?? null };
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

	/** Как `context_changed` хаба: последнее значение — расход индикатора. */
	pushContext(id: string, tokens: number | null, window: number | null): HubEvent {
		return this.hub.append(`conversation:${id}`, {
			kind: 'context_changed',
			conversation_id: id,
			payload: { tokens, window_tokens: window }
		});
	}

	/** Как эпизод сжатия хаба: строка встаёт в ленту номером события старта, тихая строка финишем. */
	pushCompaction(
		id: string,
		opts: { trigger?: string; ok?: boolean | null; pre_tokens?: number | null } = {}
	): HubEvent {
		const items = this.histories.get(id) ?? [];
		const scope = `conversation:${id}`;
		for (const item of items) {
			const at = item.kind === 'input' ? item.feed_seq || item.created_seq : item.created_seq;
			if (at && /^\d+$/.test(at)) this.hub.reach(scope, at);
		}
		const compactionId = this.id();
		const trigger = opts.trigger ?? 'auto';
		const started = this.hub.append(scope, {
			kind: 'compaction_started',
			conversation_id: id,
			payload: { id: compactionId, trigger }
		});
		const row = {
			kind: 'compaction',
			id: compactionId,
			trigger,
			pre_tokens: opts.pre_tokens ?? null,
			ok: null as boolean | null,
			created_seq: started.scope_seq,
			created_at: started.committed_at,
			finished_at: null as string | null
		} as const;
		items.push({ ...row });
		this.histories.set(id, items);
		if (opts.ok !== undefined && opts.ok !== null) return this.finishCompaction(id, compactionId, opts.ok);
		return started;
	}

	/** Закрывает уже начатый эпизод сжатия: так хаб присылает финиш. */
	finishCompaction(id: string, compactionId: string, ok = true): HubEvent {
		const items = this.histories.get(id) ?? [];
		const row = items.find((item) => item.kind === 'compaction' && item.id === compactionId);
		if (!row || row.kind !== 'compaction') throw new Error(`unknown compaction ${compactionId}`);
		row.ok = ok;
		row.finished_at = new Date().toISOString();
		return this.hub.append(`conversation:${id}`, {
			kind: 'compaction_finished',
			conversation_id: id,
			payload: { id: compactionId, trigger: row.trigger, ok }
		});
	}

	/** Как хаб в конце хода или при смерти исполнения: открытые эпизоды закрываются отказом. */
	closeOpenCompactions(id: string): HubEvent[] {
		const open = (this.histories.get(id) ?? []).filter((item) => item.kind === 'compaction' && item.ok === null);
		return open.map((row) => this.finishCompaction(id, row.id, false));
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
					revision: typeof p.revision === 'number' ? p.revision : 1,
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
				if (typeof p.revision === 'number') view.revision = p.revision;
				if (p.cause === 'edited' && Array.isArray(p.content)) {
					view.content = p.content as QueueSnapshot['queued'][number]['content'];
				}
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
			purpose: preset.purpose,
			title: preset.title,
			description: preset.description,
			revision: preset.revision,
			text_bytes: preset.text_bytes,
			created: preset.created,
			last_edit: preset.last_edit,
			used_by: preset.used_by
		};
	}

	presetList(purpose: 'instructions' | 'nudge' = 'instructions'): PresetSummary[] {
		const rank = (kind: string): number => kind === 'clean' ? 0 : kind === 'owner' ? 1 : kind === 'nudge' ? 0 : 2;
		const builtinNudge: FakePreset = {
			id: 'nudge_default',
			kind: 'nudge',
			purpose: 'nudge',
			title: 'Продолжай (английский)',
			description: 'Автоматический толчок исполнителю, который закончил ход без строки готовности.',
			revision: null,
			text_bytes: null,
			text: NUDGE_BUILTIN_TEXT,
			created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null },
			last_edit: null,
			used_by: 0
		};
		const items = [...this.presets.values()].filter((preset) => preset.purpose === purpose);
		if (purpose === 'nudge' && !items.some((preset) => preset.id === 'nudge_default')) items.push(builtinNudge);
		return items
			.sort((a, b) => rank(a.kind) - rank(b.kind) || a.title.localeCompare(b.title, 'ru'))
			.map((preset) => this.presetSummary(preset));
	}

	nudgeState(id: string): { rule: NudgeRuleView | null; events: NudgeEventView[] } {
		return {
			rule: this.nudgeRules.get(id) ?? null,
			events: [...(this.nudgeEvents.get(id) ?? [])].sort((a, b) => b.at.localeCompare(a.at)).slice(0, 5)
		};
	}

	putNudge(id: string, body: Partial<NudgeRuleView> & { paused?: boolean }): NudgeRuleView {
		const current = this.nudgeRules.get(id) ?? null;
		const paused = body.paused === true;
		const resumed = body.paused === false;
		const rule: NudgeRuleView = {
			session_id: id,
			enabled: body.enabled ?? current?.enabled ?? true,
			state: paused ? 'paused' : resumed ? 'watching' : current?.state ?? 'watching',
			idle_minutes: body.idle_minutes ?? current?.idle_minutes ?? 3,
			background_minutes: current?.background_minutes ?? 10,
			max_consecutive: body.max_consecutive ?? current?.max_consecutive ?? 3,
			max_total: current?.max_total ?? 20,
			done_markers: body.done_markers ?? current?.done_markers ?? ['STATUS: DONE', 'STATUS: BLOCKED'],
			context_warn_percent: body.context_warn_percent ?? current?.context_warn_percent ?? 80,
			set_by: current?.set_by ?? 'owner',
			set_by_session: current?.set_by_session ?? null,
			notify_session_id: current?.notify_session_id ?? null,
			consecutive: resumed ? 0 : current?.consecutive ?? 0,
			total: current?.total ?? 0,
			held: paused ? false : current?.held ?? false
		};
		this.nudgeRules.set(id, rule);
		this.hub.append(`conversation:${id}`, { kind: 'nudge_changed', conversation_id: id, payload: { ...rule, detail: 'owner' } });
		return rule;
	}

	clearNudge(id: string): void {
		const rule = this.nudgeRules.get(id);
		if (!rule) return;
		this.nudgeRules.delete(id);
		this.hub.append(`conversation:${id}`, { kind: 'nudge_changed', conversation_id: id, payload: { ...rule, state: 'exhausted', held: false, detail: 'cleared' } });
	}

	pushNudgeEvent(id: string, event: NudgeEventView, rule: NudgeRuleView): void {
		const list = this.nudgeEvents.get(id) ?? [];
		list.push(event);
		this.nudgeEvents.set(id, list);
		this.nudgeRules.set(id, rule);
		this.hub.append(`conversation:${id}`, { kind: 'nudge_changed', conversation_id: id, payload: { ...rule, detail: 'nudged' } });
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

	async history(id: string, query: { before?: string; after?: string; around?: string; limit?: number } = {}): Promise<HistoryPage> {
		return this.page(id, query);
	}

	async inputs(id: string, query: { before?: string; limit?: number } = {}): Promise<components['schemas']['OwnerInputsPage']> {
		const all = (this.histories.get(id) ?? [])
			.filter((item): item is HistoryInput => item.kind === 'input' && Boolean(item.feed_seq) && !waiting(item))
			.filter((item) => !['event', 'alarm', 'nudge'].includes((item.origin as { kind?: string } | null | undefined)?.kind ?? ''))
			.sort((a, b) => cmp(seqOf(a), seqOf(b)));
		const limit = query.limit ?? 500;
		const older = query.before ? all.filter((item) => cmp(seqOf(item), query.before!) < 0) : all;
		const slice = older.slice(-limit);
		const oldest = slice[0];
		const items = slice.map((item) => {
			const text = item.content.filter((block) => block.type === 'text').map((block) => block.text ?? '').join('');
			return {
				id: item.id,
				seq: seqOf(item),
				created_at: item.created_at,
				preview: [...text].slice(0, 200).join(''),
				attachments_count: item.content.filter((block) => block.type === 'attachment').length,
				origin: item.origin ?? null
			};
		});
		return {
			items,
			next_before: oldest && older.length > slice.length ? seqOf(oldest) : null
		};
	}

	page(id: string, query: { before?: string; after?: string; around?: string; limit?: number }): HistoryPage {
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
		if (query.around) {
			const older = all.filter((item) => cmp(seqOf(item), query.around!) <= 0);
			const newer = all.filter((item) => cmp(seqOf(item), query.around!) > 0);
			const olderLimit = limit <= 2 ? 1 : Math.max(1, Math.floor(limit / 4));
			const keptOlder = older.slice(-olderLimit);
			const keptNewer = newer.slice(0, Math.max(1, limit - keptOlder.length));
			const oldest = keptOlder[0];
			const newest = keptNewer[keptNewer.length - 1];
			return {
				items: [...keptOlder, ...keptNewer],
				next_before: oldest && older.length > keptOlder.length ? seqOf(oldest) : null,
				next_after: newest && newer.length > keptNewer.length ? seqOf(newest) : null
			};
		}
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

	async sessionUsage(id: string): Promise<components['schemas']['SessionUsageView']> {
		const empty = {
			turns: 0,
			input_tokens: 0,
			output_tokens: 0,
			cache_read_tokens: 0,
			cache_write_tokens: 0,
			models: []
		};
		return this.usage.get(id) ?? { session: empty, delegates: { ...empty } };
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
