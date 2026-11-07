import { BlockModel } from '../../../../src/lib/stores/blocks';
import { cardCall, type CallFacts } from '../../../../src/lib/timeline/cards/call';
import type { CardCall } from '../../../../src/lib/timeline/cards/types';

let serial = 0;

export function callBlock(toolName: string, input: unknown, patch: Partial<BlockModel> = {}): BlockModel {
  const block = new BlockModel(`call-${++serial}`, 'tool_call');
  block.toolName = toolName;
  block.agent = 'orchestrator';
  block.text = typeof input === 'string' ? input : JSON.stringify(input);
  block.state = 'final';
  block.createdAt = Date.parse('2026-11-07T10:00:00Z');
  block.startedAt = block.createdAt;
  block.finishedAt = block.createdAt;
  Object.assign(block, patch);
  return block;
}

export function resultBlock(text: string, patch: Partial<BlockModel> = {}): BlockModel {
  const block = new BlockModel(`result-${++serial}`, 'tool_result');
  block.text = text;
  block.state = 'final';
  block.createdAt = Date.parse('2026-11-07T10:00:01Z');
  block.finishedAt = block.createdAt;
  Object.assign(block, patch);
  return block;
}

/** Ответ хаба, как его печатает воркер: JSON с отступами и подсказка отдельной строкой. */
export function hub(value: unknown, hint = ''): string {
  return JSON.stringify(value, null, 2) + (hint ? `\n${hint}` : '');
}

export function made(toolName: string, input: unknown, result: string | null, facts: CallFacts = {}): CardCall {
  return cardCall(callBlock(toolName, input), result === null ? null : resultBlock(result), { root: true, ...facts });
}
