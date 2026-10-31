import type { BlockModel } from './blocks';

export interface DelegationNode {
	block_id: string;
	parent_block_id: string | null;
	started_at: string;
	finished_at: string | null;
	state: string;
	tool_name: string | null;
	task: string | null;
	subagent: { name: string; model: string } | null;
}

function iso(ms: number | null): string | null {
	if (ms === null) return null;
	return new Date(ms).toISOString();
}

const DELEGATING = new Set(['agent', 'task', 'spawn_agent', 'subagent', 'delegate']);

const hubState = (state: string): string => (state === 'streaming' ? 'running' : state === 'final' ? 'done' : 'incomplete');

/**
 * Вызовы субагентов по блокам ленты, как их отдаёт хаб: вызов делегирования, имя и модель
 * берутся у первого дочернего блока, состояние приводится к running / done / incomplete.
 */
export function buildDelegation(blocks: Iterable<BlockModel>): DelegationNode[] {
	const list = [...blocks];
	const children = new Map<string, BlockModel[]>();
	for (const block of list) {
		if (block.parentId === null) continue;
		const bucket = children.get(block.parentId) ?? [];
		bucket.push(block);
		children.set(block.parentId, bucket);
	}
	const order = (a: BlockModel, b: BlockModel): number => {
		const left = BigInt(a.createdSeq);
		const right = BigInt(b.createdSeq);
		return left < right ? -1 : left > right ? 1 : a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
	};
	return list
		.filter(
			(block) =>
				block.kind === 'tool_call' &&
				(DELEGATING.has((block.toolName ?? '').toLowerCase()) || (children.get(block.id) ?? []).some((child) => child.agent !== block.agent))
		)
		.sort(order)
		.map((block) => {
			const first = [...(children.get(block.id) ?? [])].sort(order)[0];
			return {
				block_id: block.id,
				parent_block_id: block.parentId,
				started_at: iso(block.startedAt) ?? new Date(block.createdAt).toISOString(),
				finished_at: iso(block.finishedAt),
				state: hubState(block.state),
				tool_name: block.toolName,
				task: block.summary,
				subagent: first ? { name: first.agent, model: first.model } : null
			};
		});
}

export function laneBlocks(blocks: Iterable<BlockModel>, rootBlockId: string): BlockModel[] {
	const list = [...blocks];
	const children = new Map<string, BlockModel[]>();
	for (const block of list) {
		if (!block.parentId) continue;
		const bucket = children.get(block.parentId) ?? [];
		bucket.push(block);
		children.set(block.parentId, bucket);
	}
	const root = list.find((block) => block.id === rootBlockId);
	if (!root) return [];
	const out: BlockModel[] = [];
	const walk = (block: BlockModel): void => {
		out.push(block);
		for (const child of children.get(block.id) ?? []) walk(child);
	};
	walk(root);
	return out;
}

const VERBS: Record<string, string> = {
	Read: 'Читает',
	Grep: 'Ищет',
	Glob: 'Ищет',
	Bash: 'Выполняет',
	Edit: 'Правит',
	Write: 'Правит',
	Agent: 'Делегирует'
};

export function subagentVerb(toolName: string | null): string {
	if (!toolName) return 'Работает';
	return VERBS[toolName] ?? 'Работает';
}
