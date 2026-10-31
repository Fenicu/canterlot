import { sha256Hex } from '../../src/lib/stores/blocks';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';
import rows from './first-dialog.json' with { type: 'json' };

/** Строки настоящего первого диалога с продакшена в порядке ленты. */
export const FIRST_DIALOG = rows as unknown as HistoryItem[];

const blocks = (): HistoryBlock[] => FIRST_DIALOG.flatMap((item) => (item.kind === 'block' ? [item] : []));

export const firstDialogBlock = (match: (block: HistoryBlock) => boolean): HistoryBlock => {
	const found = blocks().find(match);
	if (!found) throw new Error('first dialog has no such block');
	return found;
};

export function dialogConversation(api: FakeHubApi): string {
	return seedConversation(api, {
		title: 'Привет! это первый тест из кантерлота!',
		model: 'opus[1m]',
		harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
		machine: { id: api.id(), name: 'dev-vm' }
	});
}

/** История целиком, как после перезагрузки страницы. */
export function seedFirstDialog(api: FakeHubApi): string {
	const id = dialogConversation(api);
	api.histories.set(id, structuredClone(FIRST_DIALOG));
	for (const block of blocks()) api.blocks.set(block.id, { text: block.content, rev: block.rev, state: block.state });
	api.bindConversation(id);
	return id;
}

function inputOf(item: HistoryInput) {
	return item.content.flatMap((part) => (part.type === 'text' ? [part.text ?? ''] : [])).join('');
}

/** Тот же диалог живыми событиями: страница уже открыта, блоки приходят по одному. */
export async function playFirstDialog(api: FakeHubApi, id: string): Promise<void> {
	const scope = `conversation:${id}`;
	const open: string[] = [];
	const append = (kind: string, payload: unknown, at: string): void => {
		api.hub.append(scope, { kind, conversation_id: id, payload, at });
	};
	for (const item of FIRST_DIALOG) {
		if (item.kind === 'input') {
			for (const block of open.splice(0)) append('block_incomplete', { block_id: block, rev: 1, reason: 'turn_ended' }, item.created_at);
			const input_id = item.id;
			append('input_queued', { input_id, idempotency_key: item.idempotency_key, input_seq: item.created_seq, state: 'queued', content: [{ type: 'text', text: inputOf(item) }] }, item.created_at);
			append('input_state_changed', { input_id, state: 'accepted', cause: 'accepted', attempt: 1 }, item.created_at);
			continue;
		}
		if (item.kind !== 'block') continue;
		const block = item;
		api.blocks.set(block.id, { text: block.content, rev: block.rev, state: block.state });
		append(
			'block_started',
			{
				block_id: block.id,
				rev: 0,
				kind: block.block_kind,
				input_command_id: block.input_command_id ?? null,
				parent_block_id: block.parent_block_id ?? null,
				agent: block.agent ?? '',
				model: block.model ?? '',
				tool_name: block.tool_name ?? null,
				summary: block.block_kind === 'tool_call' ? null : (block.summary ?? null),
				approved_by: null
			},
			block.created_at
		);
		if (block.state === 'incomplete') {
			open.push(block.id);
			continue;
		}
		append(
			'block_final',
			{ block_id: block.id, rev: 1, text: block.content, checksum: await sha256Hex(block.content), text_omitted: false, summary: block.summary ?? null },
			block.finished_at ?? block.created_at
		);
	}
	for (const block of open.splice(0)) append('block_incomplete', { block_id: block, rev: 1, reason: 'turn_ended' }, '2026-10-31T10:24:25.200Z');
	api.hub.flush();
}
