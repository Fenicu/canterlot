import type { BackgroundFact } from '#lib/stores/background.svelte.js';
import type { BlockModel } from '#lib/stores/blocks.js';
import { stripAnsi } from '#lib/render/ansi.js';
import { cleanToolOutput, isLaunchResult, isMainAgent, toolError, toolInterrupted, toolTiming, toolTitle } from '#lib/timeline/present.js';
import type { HubEvent } from '#lib/timeline/hubEvent.js';
import { isObject, jsonObjectIn, parseJson, type JsonObject } from './json';
import { toolId } from './toolId';
import type { CardCall, CardState, SessionFacts, Voice } from './types';

export interface CallFacts {
  working?: boolean;
  background?: BackgroundFact | null;
  /** Лента корневой сессии владельца: её агент говорит о себе в женском роде. */
  root?: boolean;
  harness?: string;
  session?: (id: string) => SessionFacts | null;
  events?: (sessionId: string) => HubEvent[];
  previous?: () => BlockModel | null;
}

/** Состояние вызова из тех же признаков, что у обычного блока инструмента. */
export function cardState(block: BlockModel, result: BlockModel | null, facts: CallFacts = {}): CardState {
  const background = facts.background ?? null;
  const timing = toolTiming(block, result, facts.working ?? false, background);
  const error = toolError(block, result) || background?.state === 'failed';
  if (error) return 'error';
  if (timing.running || block.state === 'stale' || block.state === 'streaming') return 'running';
  if (block.state === 'incomplete' || background?.state === 'stopped' || toolInterrupted(result)) return 'interrupted';
  return 'done';
}

export function voiceOf(block: Pick<BlockModel, 'agent'>, root: boolean): Voice {
  return root && isMainAgent(block.agent) ? 'she' : 'neutral';
}

/** Вызов, разобранный для карточки: вход, ответ, состояние и голос. */
export function cardCall(block: BlockModel, result: BlockModel | null, facts: CallFacts = {}): CardCall {
  // Пока вызов идёт, текст блока у Claude Code — это сводка, а не вход.
  const text = block.state === 'final' || block.state === 'incomplete' ? block.text : '';
  const parsed = parseJson(text);
  const input: JsonObject = isObject(parsed.value) ? parsed.value : {};
  const finished = result && result.state !== 'streaming' && !isLaunchResult(result) ? result : null;
  const output = finished ? cleanToolOutput(stripAnsi(finished.text)).trim() : '';
  return {
    id: toolId(block.toolName),
    name: block.toolName ?? '',
    block,
    result,
    input,
    partial: parsed.partial,
    hasInput: isObject(parsed.value),
    argument: toolTitle(block).argument,
    output,
    answer: output ? jsonObjectIn(output) : null,
    state: cardState(block, result, facts),
    voice: voiceOf(block, facts.root ?? false),
    harness: facts.harness ?? '',
    session: facts.session ?? (() => null),
    events: facts.events ?? (() => []),
    previous: facts.previous ?? (() => null)
  };
}
