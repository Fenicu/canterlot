import { sha256Hex } from '../../src/lib/stores/blocks';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { DelegationNode } from '../../src/lib/stores/delegation';
import type { WindowForecast } from '../../src/lib/stores/limits';
import type { ListRow } from '../../src/lib/stores/list.svelte';
import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import { FakeHubApi, type FakePreset } from './api';

const idle = { state: 'idle' as const, evidence: {}, since: '2026-10-28T12:00:00.000Z' };

export function seedConversation(api: FakeHubApi, patch: Partial<ListRow> = {}): string {
	const { id: given, ...rest } = patch;
	const id = given ?? api.id();
	const row: ListRow = {
		id,
		title: patch.title ?? 'Сессия',
		title_source: 'user',
		color: null,
		pinned: false,
		archived: false,
		accepted: false,
		groups: [],
		group_pins: [],
		parent_id: null,
		spawn_depth: 0,
		machine: { id: api.id(), name: 'dev' },
		harness: { id: 'claude', title: 'Claude', provider: 'anthropic' },
		workdir: '/work',
		created_at: '2026-10-28T12:00:00.000Z',
		model: 'opus',
		effort: 'medium',
		permission_mode: 'owner',
		activity: idle,
		unread_count: 0,
		queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
		crunch: { enabled: false },
		last_activity_at: null,
		routing: null,
		...rest
	};
	api.list.set(id, row);
	api.details.set(id, { ...row });
	api.counts = {
		all: [...api.list.values()].filter((item) => !item.archived && !item.trashed_at).length,
		pinned: [...api.list.values()].filter((item) => item.pinned && !item.archived && !item.trashed_at).length,
		archived: [...api.list.values()].filter((item) => item.archived && !item.accepted && !item.trashed_at).length,
		trashed: [...api.list.values()].filter((item) => item.trashed_at).length,
		groups: api.counts.groups
	};
	api.bindConversation(id);
	api.hub.append('conversations', {
		kind: 'conversation_created',
		conversation_id: id,
		payload: { ...row, counts: api.counts }
	});
	return id;
}

export function seedHistory(api: FakeHubApi, conversationId: string, n: number): HistoryBlock[] {
	const items: HistoryBlock[] = [];
	for (let i = 1; i <= n; i++) {
		const blockId = api.id();
		const item: HistoryBlock = {
			kind: 'block',
			id: blockId,
			block_kind: 'text',
			content: `строка ${i}`,
			content_bytes: `строка ${i}`.length,
			content_truncated: false,
			created_seq: String(i),
			created_at: '2026-10-28T12:00:00.000Z',
			rev: 1,
			state: 'final',
			agent: 'agent',
			model: 'opus',
			tool_name: null,
			summary: null,
			parent_block_id: null,
			input_command_id: null,
			line_count: 1
		};
		items.push(item);
		api.blocks.set(blockId, { text: item.content, rev: 1, state: 'final' });
	}
	api.histories.set(conversationId, items);
	api.bindConversation(conversationId);
	return items;
}

export function seedInput(api: FakeHubApi, conversationId: string, input: HistoryInput): void {
	const items = api.histories.get(conversationId) ?? [];
	items.push(input);
	api.histories.set(conversationId, items);
}

export async function streamAnswer(
	api: FakeHubApi,
	conversationId: string,
	text: string,
	opts: { chunk?: number; blockId?: string } = {}
): Promise<{ blockId: string; checksum: string }> {
	const blockId = opts.blockId ?? api.id();
	const scope = `conversation:${conversationId}`;
	const chunk = opts.chunk ?? 12;
	api.hub.append(scope, {
		kind: 'block_started',
		conversation_id: conversationId,
		payload: {
			block_id: blockId,
			rev: 0,
			kind: 'text',
			input_command_id: null,
			parent_block_id: null,
			agent: 'agent',
			model: 'opus',
			tool_name: null,
			summary: null,
			approved_by: null
		}
	});
	let rev = 0;
	for (let i = 0; i < text.length; ) {
		const append = text.slice(i, i + chunk);
		i += append.length;
		rev += 1;
		api.hub.append(scope, {
			kind: 'block_delta',
			conversation_id: conversationId,
			payload: { block_id: blockId, base_rev: rev - 1, rev, append }
		});
	}
	const checksum = await sha256Hex(text);
	api.blocks.set(blockId, { text, rev, state: 'streaming' });
	return { blockId, checksum };
}

export function seedMachine(api: FakeHubApi, patch: Partial<MachineRecord> = {}): MachineRecord {
	const machine: MachineRecord = {
		id: patch.id ?? api.id(),
		name: patch.name ?? 'dev',
		availability: patch.availability ?? 'available',
		os: patch.os ?? 'linux',
		warnings: patch.warnings ?? [],
		metrics: patch.metrics ?? { observed_at: '2026-10-28T12:00:00.000Z' },
		home_dir: patch.home_dir ?? null,
		lease: patch.lease ?? null,
		harnesses: patch.harnesses ?? [
			{
				id: 'claude',
				title: 'Claude',
				provider: 'anthropic',
				models: [{ id: 'opus', title: 'Opus', efforts: ['low', 'medium'] }],
				permission_modes: ['owner']
			}
		],
	};
	api.machines.set(machine.id, machine);
	return machine;
}

type LeaseState = NonNullable<MachineRecord['lease']>;
type LeaseEntry = LeaseState['holders'][number];

/** Запись очереди тяжёлых задач, как её отдаёт хаб: держатель с `since`, ожидающий с `position`. */
export function leaseEntry(patch: Partial<LeaseEntry> & { lease_id: string }): LeaseEntry {
	return {
		kind: 'build', weight: patch.kind === 'gate' ? 2 : 1, label: '', holder: patch.conversation_id ? 'execution' : 'owner',
		execution_id: null, conversation_id: null, since: null, requested_at: null, position: null, dir: '', ...patch
	};
}

/** Полное состояние очереди машины в снимке фейкового хаба. */
export function seedLease(api: FakeHubApi, machineId: string, holders: LeaseEntry[], waiters: LeaseEntry[] = [], slots = 2): LeaseState {
	const lease: LeaseState = { slots, observed_at: new Date().toISOString(), holders, waiters };
	const machine = api.machines.get(machineId);
	if (machine) machine.lease = lease;
	return lease;
}

export function rememberDelegation(api: FakeHubApi, conversationId: string, nodes: DelegationNode[]): void {
	api.delegations.set(conversationId, nodes);
}

const SEED_EXECUTOR_TEXT = 'Communicate, reason and write reports in English. Artefacts whose language the task fixes (commit messages, UI strings, documentation) follow the task.';

export function seedPresets(api: FakeHubApi): void {
	if (api.presets.size > 0) return;
	const clean: FakePreset = {
		id: 'clean',
		kind: 'clean',
		purpose: 'instructions',
		title: 'Без инструкций',
		description: 'Агент запускается без личных инструкций владельца: без его глобальных CLAUDE.md и AGENTS.md и без автопамяти. Инструкции самого репозитория, вход, MCP-серверы и плагины владельца остаются.',
		revision: null,
		text_bytes: null,
		text: null,
		created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null },
		last_edit: null,
		used_by: 0
	};
	const owner: FakePreset = {
		id: 'owner',
		kind: 'owner',
		purpose: 'instructions',
		title: 'Как у владельца',
		description: 'Конфигурация агента владельца без изменений: его CLAUDE.md, AGENTS.md и автопамять.',
		revision: null,
		text_bytes: null,
		text: null,
		created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null },
		last_edit: null,
		used_by: 0
	};
	const executor: FakePreset = {
		id: 'seed-executor',
		kind: 'custom',
		purpose: 'instructions',
		title: 'Исполнитель (английский)',
		description: 'Общение, рассуждения и отчёты на английском; язык коммитов, интерфейса и документации задаёт задача.',
		revision: 1,
		text_bytes: new TextEncoder().encode(SEED_EXECUTOR_TEXT).length,
		text: SEED_EXECUTOR_TEXT,
		created: { by: 'owner', conversation_id: null, conversation_title: null, at: '2026-10-28T12:00:00.000Z' },
		last_edit: null,
		used_by: 0
	};
	api.presets.set(clean.id, clean);
	api.presets.set(owner.id, owner);
	api.presets.set(executor.id, executor);
	const draftText = 'Draft notes kept by the agent.';
	const agentDraft: FakePreset = {
		id: 'agent-draft',
		kind: 'custom',
		purpose: 'instructions',
		title: 'Агентский черновик',
		description: 'Сохранено агентом из сессии.',
		revision: 2,
		text_bytes: new TextEncoder().encode(draftText).length,
		text: draftText,
		created: { by: 'agent', conversation_id: '33333333-3333-4333-8333-333333333333', conversation_title: 'Разбор ленты', at: '2026-10-29T10:00:00.000Z' },
		last_edit: { by: 'agent', conversation_id: '33333333-3333-4333-8333-333333333333', conversation_title: 'Разбор ленты', at: '2026-10-29T11:00:00.000Z' },
		used_by: 1
	};
	api.presets.set(agentDraft.id, agentDraft);
}

/**
 * Прогноз окна, как его считает хаб: средняя скорость в промилле за час и момент исчерпания.
 * `exhaustsInMs: null` — скорость нулевая; `before_reset` задаётся явно, как в поле хаба.
 */
export function limitForecast(opts: { rate: number; recent?: number | null; exhaustsInMs: number | null; beforeReset: boolean; now?: number }): WindowForecast {
	const now = opts.now ?? Date.now();
	return {
		rate_permille_per_hour: opts.rate,
		recent_rate_permille_per_hour: opts.recent ?? null,
		exhausts_at: opts.exhaustsInMs === null ? null : new Date(now + opts.exhaustsInMs).toISOString(),
		before_reset: opts.beforeReset
	};
}

export interface MachineWatchSeed {
	watch: NonNullable<MachineRecord['watch']>;
	memory_low: boolean;
	mem_available_bytes: number;
}

/**
 * Отчёт сторожа для экрана машин: крупные кэши сборки при нехватке диска, мало памяти,
 * три процесса из двух закрытых сессий и две сборки мимо очереди.
 */
export function machineWatchSeed(api: FakeHubApi, now = Date.now()): MachineWatchSeed {
	const ago = (ms: number) => new Date(now - ms).toISOString();
	const HOUR = 3_600_000;
	const MIB = 1024 ** 2;
	const banner = api.id();
	const forecast = api.id();
	// Похожие на настоящие UUIDv7: подпись запуска без названия берёт хвост идентификатора.
	const bannerRun = '019a6c1b-2d90-7e11-8c4f-a3b6e0d29174';
	const forecastRun = '019a6c2e-7f41-7d3a-9b2c-5e81d4a7c3f9';
	return {
		memory_low: true,
		mem_available_bytes: Math.round(2.1 * 1024 ** 3),
		watch: {
			observed_at: ago(20_000),
			hotspots: [
				{ path: '/home/owner/myprojects/canterlot-2a/web/node_modules', kind: 'node_modules', bytes: 1_200_000_000, incremental_bytes: 0, newest_at: ago(26 * HOUR), measured_at: ago(60_000), conversation_id: null, title: null },
				{ path: '/home/owner/myprojects/canterlot-2a/target', kind: 'target', bytes: 77_000_000_000, incremental_bytes: 31_000_000_000, newest_at: ago(2 * HOUR), measured_at: ago(60_000), conversation_id: null, title: null },
				{ path: '/home/owner/.local/state/canterlot-worker/worktrees/01a1b8e2-659f-715f-aad1-bf53d98aefdd/target', kind: 'target', bytes: 12_000_000_000, incremental_bytes: 4_300_000_000, newest_at: ago(5 * HOUR), measured_at: ago(60_000), conversation_id: banner, title: 'Баннер связи' },
				{ path: '/home/owner/myprojects/canterlot-2a/web/.svelte-kit', kind: 'svelte_kit', bytes: 380_000_000, incremental_bytes: 0, newest_at: ago(2 * HOUR), measured_at: ago(60_000), conversation_id: null, title: null }
			],
			leaked: [
				{ execution_id: bannerRun, conversation_id: banner, title: 'Баннер связи', pid: 48211, comm: 'cargo', rss_bytes: 600 * MIB, age_s: 3 * 3600 + 40 * 60 },
				{ execution_id: bannerRun, conversation_id: banner, title: 'Баннер связи', pid: 48390, comm: 'cargo', rss_bytes: 380 * MIB, age_s: 3 * 3600 + 38 * 60 },
				{ execution_id: forecastRun, conversation_id: forecast, title: null, pid: 51022, comm: 'node', rss_bytes: 450 * MIB, age_s: 50 * 60 }
			],
			unleased_count: 2
		}
	};
}

/** Кладёт отчёт сторожа в запись машины так, как его отдаёт снимок `machines`. */
export function seedMachineWatch(api: FakeHubApi, machineId: string, now = Date.now()): MachineWatchSeed {
	const seed = machineWatchSeed(api, now);
	const machine = api.machines.get(machineId);
	if (!machine) throw new Error(`no machine ${machineId}`);
	const kept = machine.warnings.filter(warning => warning !== 'memory_low' && warning !== 'processes_leaked');
	machine.watch = seed.watch;
	machine.warnings = [...new Set([...kept, 'disk_usage_high', 'memory_low', 'processes_leaked'])];
	if (machine.metrics) machine.metrics = { ...machine.metrics, mem_available_bytes: seed.mem_available_bytes };
	return seed;
}
