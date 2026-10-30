import { sha256Hex } from '../../src/lib/stores/blocks';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { DelegationNode } from '../../src/lib/stores/delegation';
import type { ListRow } from '../../src/lib/stores/list.svelte';
import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import { FakeHubApi } from './api';

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
		groups: [],
		machine: { id: api.id(), name: 'dev' },
		harness: { id: 'claude', title: 'Claude', provider: 'anthropic' },
		workdir: '/work',
		created_at: '2026-10-28T12:00:00.000Z',
		model: 'opus',
		effort: 'medium',
		permission_mode: 'default',
		activity: idle,
		unread_count: 0,
		queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
		crunch: { enabled: false },
		last_activity_at: null,
		...rest
	};
	api.list.set(id, row);
	api.details.set(id, { ...row });
	api.counts = {
		all: [...api.list.values()].filter((item) => !item.archived).length,
		pinned: [...api.list.values()].filter((item) => item.pinned && !item.archived).length,
		archived: [...api.list.values()].filter((item) => item.archived).length,
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
		harnesses: patch.harnesses ?? [
			{
				id: 'claude',
				title: 'Claude',
				provider: 'anthropic',
				models: [{ id: 'opus', title: 'Opus', efforts: ['low', 'medium'] }],
				permission_modes: ['default']
			}
		],
	};
	api.machines.set(machine.id, machine);
	return machine;
}

export function rememberDelegation(api: FakeHubApi, conversationId: string, nodes: DelegationNode[]): void {
	api.delegations.set(conversationId, nodes);
}
