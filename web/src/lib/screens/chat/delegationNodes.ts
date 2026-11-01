import { executionLive, type ChatStore } from '#lib/stores/chat.svelte.js';
import { buildDelegation, type DelegationNode } from '#lib/stores/delegation.js';

/** У этих CLI вызов субагента финален сразу, а состояние идёт от результата. */
const TRACKED = new Set(['claude_code', 'codex']);

/** Узлы делегирования по блокам ленты; читать только внутри $derived. */
export function liveNodes(chat: ChatStore): DelegationNode[] {
	const harness = (chat.detail?.harness as { id?: string } | undefined)?.id ?? '';
	return buildDelegation(chat.blocks.values(), TRACKED.has(harness) ? { background: chat.backgroundFacts, live: executionLive(chat.execution) } : undefined);
}
