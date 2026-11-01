import type { BackgroundFact } from './background.svelte';
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

/** Как у хаба: вызов субагента финален сразу, его состояние читается отсюда. */
export interface SubagentFacts {
	background: ReadonlyMap<string, BackgroundFact>;
	/** Исполнение живо: вызов без результата ещё работает. */
	live: boolean;
}

const TASK_STATE: Record<string, string> = { completed: 'done', failed: 'failed', stopped: 'stopped' };

function subagentState(block: BlockModel, result: BlockModel | undefined, facts: SubagentFacts): { state: string; finishedAt: number | null } {
	const task = facts.background.get(block.id);
	if (task) {
		if (task.state === 'running') return { state: 'running', finishedAt: null };
		return { state: TASK_STATE[task.state] ?? 'stopped', finishedAt: task.endedAt ?? block.finishedAt };
	}
	if (result) {
		if (result.state === 'streaming') return { state: 'running', finishedAt: null };
		const state = result.state !== 'final' ? 'incomplete' : (RESULT_STATE[result.summary ?? ''] ?? 'done');
		return { state, finishedAt: result.finishedAt ?? block.finishedAt };
	}
	if (hubState(block.state) === 'incomplete' || !facts.live) return { state: 'incomplete', finishedAt: block.finishedAt };
	return { state: 'running', finishedAt: null };
}

const SUBAGENT_CALLS = new Set(['agent', 'task', 'spawn_agent']);
const RESULT_STATE: Record<string, string> = { error: 'failed', interrupted: 'stopped' };
const TOOL_LAUNCHED = 'async_launched';

/**
 * Вызовы субагентов по блокам ленты, как их отдаёт хаб: вызов делегирования, имя и модель
 * берутся у первого дочернего блока другого агента. Без `facts` состояние вызова приводится к running / done /
 * incomplete, с ними у вызовов Agent и Task идёт от фоновой задачи и настоящего результата.
 */
export function buildDelegation(blocks: Iterable<BlockModel>, facts?: SubagentFacts): DelegationNode[] {
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
			const own = [...(children.get(block.id) ?? [])].sort(order);
			// Первым идёт результат запуска основного агента: субагента называют его собственные блоки.
			const first = own.find((child) => child.agent !== block.agent);
			const result = own.findLast((child) => child.kind === 'tool_result' && child.summary !== TOOL_LAUNCHED);
			const tracked = facts && SUBAGENT_CALLS.has((block.toolName ?? '').toLowerCase());
			const end = tracked ? subagentState(block, result, facts) : { state: hubState(block.state), finishedAt: block.finishedAt };
			return {
				block_id: block.id,
				parent_block_id: block.parentId,
				started_at: iso(block.startedAt) ?? new Date(block.createdAt).toISOString(),
				finished_at: iso(end.finishedAt),
				state: end.state,
				tool_name: block.toolName,
				task: block.summary,
				subagent: first ? { name: first.agent, model: first.model } : null
			};
		});
}

const FINISHED = new Set(['done', 'final', 'completed', 'failed', 'error', 'aborted', 'stopped', 'incomplete']);
const INTERRUPTED = new Set(['failed', 'error', 'aborted', 'stopped', 'incomplete']);

export function isRunning(node: DelegationNode): boolean {
	return !FINISHED.has(node.state);
}

export function runningCount(nodes: readonly DelegationNode[]): number {
	return nodes.filter(isRunning).length;
}

export function interruptedCount(nodes: readonly DelegationNode[]): number {
	return nodes.filter((node) => INTERRUPTED.has(node.state)).length;
}

/** Снимок хаба дополняется живыми узлами: у одного блока побеждает живой. */
export function mergeNodes(server: readonly DelegationNode[], live: readonly DelegationNode[]): DelegationNode[] {
	const map = new Map<string, DelegationNode>();
	for (const node of server) map.set(node.block_id, node);
	for (const node of live) map.set(node.block_id, node);
	return [...map.values()];
}

/**
 * Начало последнего хода: хода (`inputCommandId`) у самого нового блока, от первого блока этого хода.
 * Фоновая работа, разбудившая сессию, получает ход последней команды, так что границы хода едины для всех.
 */
export function latestTurnStart(blocks: Iterable<BlockModel>): number | null {
	let newest: BlockModel | null = null;
	const list: BlockModel[] = [];
	for (const block of blocks) {
		if (block.inputCommandId === null) continue;
		list.push(block);
		if (!newest || BigInt(block.createdSeq) > BigInt(newest.createdSeq)) newest = block;
	}
	if (!newest) return null;
	const run = newest.inputCommandId;
	let start = Infinity;
	for (const block of list) if (block.inputCommandId === run) start = Math.min(start, block.startedAt ?? block.createdAt);
	return start;
}

export interface DelegationGroups {
	/** Работающие, затем закончившие в последнем ходе. */
	shown: DelegationNode[];
	/** Всё остальное завершённое, новые сверху. */
	older: DelegationNode[];
	/** Сколько из `older` оборвано или упало. */
	interrupted: number;
}

const stamp = (value: string | null): number => (value ? Date.parse(value) : 0);
const endOf = (node: DelegationNode): number => stamp(node.finished_at ?? node.started_at);

export function groupDelegation(nodes: readonly DelegationNode[], turnStart: number | null): DelegationGroups {
	const running = nodes.filter(isRunning).sort((a, b) => stamp(a.started_at) - stamp(b.started_at));
	const done = nodes.filter((node) => !isRunning(node));
	const fresh = (node: DelegationNode): boolean => turnStart !== null && endOf(node) >= turnStart;
	const recent = done.filter(fresh).sort((a, b) => stamp(a.started_at) - stamp(b.started_at));
	const older = done.filter((node) => !fresh(node)).sort((a, b) => endOf(b) - endOf(a));
	return { shown: [...running, ...recent], older, interrupted: interruptedCount(older) };
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
