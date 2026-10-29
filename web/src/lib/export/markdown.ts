import type { BlockModel } from '$lib/stores/blocks';

export function answerToMarkdown(blocks: BlockModel[]): string {
	const lines: string[] = [];
	const mainAgent = blocks.find((b) => b.parentId === null && b.toolName !== 'Agent')?.agent;

	for (const block of blocks) {
		// Omit subagents (blocks with a parentId, or Agent delegation tools, or subagent tasks)
		if (
			block.parentId !== null ||
			block.toolName === 'Agent' ||
			(mainAgent && block.agent && block.agent !== mainAgent)
		) {
			continue;
		}

		if (block.kind === 'text') {
			const text = (block.body || block.text || '').trim();
			if (text) {
				lines.push(text);
			}
		} else if (block.kind === 'tool_call') {
			const toolName = block.toolName || 'Tool';
			const summary = block.summary ? `: ${block.summary}` : '';
			lines.push(`_${toolName}${summary}_`);
		}
	}

	return lines.join('\n\n');
}
