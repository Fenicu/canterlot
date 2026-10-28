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

export function buildDelegation(blocks: Iterable<BlockModel>): DelegationNode[] {
	const list = [...blocks];
	const parents = new Set(list.map((block) => block.parentId).filter((id): id is string => id !== null));
	return list
		.filter((block) => block.parentId !== null || parents.has(block.id) || block.toolName === 'Agent')
		.map((block) => ({
			block_id: block.id,
			parent_block_id: block.parentId,
			started_at: iso(block.startedAt) ?? new Date(block.createdAt).toISOString(),
			finished_at: iso(block.finishedAt),
			state: block.state,
			tool_name: block.toolName,
			task: block.summary,
			subagent:
				block.toolName === 'Agent' ? { name: block.agent, model: block.model } : null
		}));
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
