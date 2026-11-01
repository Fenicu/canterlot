import { executionLive, type ChatStore } from '#lib/stores/chat.svelte.js';
import { buildDelegation, type DelegationNode } from '#lib/stores/delegation.js';

/** Узлы делегирования по блокам ленты; читать только внутри $derived. */
export function liveNodes(chat: ChatStore): DelegationNode[] {
	const claudeCode = (chat.detail?.harness as { id?: string } | undefined)?.id === 'claude_code';
	return buildDelegation(chat.blocks.values(), claudeCode ? { background: chat.backgroundFacts, live: executionLive(chat.execution) } : undefined);
}
