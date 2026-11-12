/** Что известно полю ввода, чтобы решить, показывать ли подсказку следующего сообщения. */
export interface SuggestionInput {
  suggestion: string | null;
  text: string;
  /** Файлы в лотке, включая загружаемые. */
  attachments: number;
  uploadsPending: boolean;
  /** `detail.harness.id`: подсказки бывают только у Claude Code. */
  harness: string | undefined;
  /** `activity.state` разговора. */
  activity: unknown;
  runSlotFree: boolean;
}

/**
 * Подсказка видна только в совсем пустом поле, у Claude Code и между ходами.
 * Черновик всегда важнее: любой символ или файл её скрывает.
 */
export function visibleSuggestion(input: SuggestionInput): string | null {
  const text = input.suggestion?.trim() ?? '';
  if (text === '') return null;
  if (input.text !== '' || input.attachments > 0 || input.uploadsPending) return null;
  if (input.harness !== 'claude_code') return null;
  if (input.activity === 'working' || !input.runSlotFree) return null;
  return text;
}

/** Tab подставляет подсказку только без модификаторов и вне набора IME. */
export function acceptsWithTab(event: { key: string; shiftKey: boolean; ctrlKey: boolean; metaKey: boolean; altKey: boolean; isComposing: boolean }): boolean {
  return event.key === 'Tab' && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey && !event.isComposing;
}
