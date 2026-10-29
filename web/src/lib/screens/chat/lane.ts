import type { BlockModel } from '$lib/stores/blocks';

export interface LaneModelResult {
	title: string;
	subtitle: string;
	task: string | null;
	status: string;
	running: boolean;
	breadcrumb: string[];
}

export function laneModel(
	blocks: BlockModel[],
	rootBlockId: string,
	opts?: { machineName?: string }
): LaneModelResult {
	const root = blocks.find((b) => b.id === rootBlockId);
	const agentName = root?.agent || 'Explore';
	const modelName = root?.model || 'haiku';
	const machine = opts?.machineName || 'workstation';
	const task = root?.summary ?? null;

	const title = task || agentName;
	const subtitle = `Субагент · ${agentName} · ${modelName} · ${machine}`;
	const breadcrumb = ['Оркестратор', agentName];

	let status = 'Готово';
	let running = false;

	if (root) {
		const isStreaming =
			root.state === 'streaming' ||
			blocks.some((b) => b.parentId === rootBlockId && b.state === 'streaming');

		if (isStreaming) {
			status = 'Работает параллельно с основным агентом';
			running = true;
		} else if (root.state === 'incomplete' || root.state === 'stale') {
			status = 'Прервано';
			running = false;
		} else {
			status = 'Готово';
			running = false;
		}
	}

	return {
		title,
		subtitle,
		task,
		status,
		running,
		breadcrumb
	};
}
