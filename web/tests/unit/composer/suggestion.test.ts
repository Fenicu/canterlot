import { describe, expect, it } from 'vitest';
import { acceptsWithTab, visibleSuggestion, type SuggestionInput } from '#lib/composer/suggestion.js';

const base: SuggestionInput = {
  suggestion: 'добавь тесты',
  text: '',
  attachments: 0,
  uploadsPending: false,
  harness: 'claude_code',
  activity: 'idle',
  runSlotFree: true
};

describe('visibleSuggestion', () => {
  it('shows the suggestion in a completely empty field between turns', () => {
    expect(visibleSuggestion(base)).toBe('добавь тесты');
  });

  it('draft always wins: text, a file or an upload hides it', () => {
    expect(visibleSuggestion({ ...base, text: 'д' })).toBeNull();
    expect(visibleSuggestion({ ...base, text: ' ' })).toBeNull();
    expect(visibleSuggestion({ ...base, attachments: 1 })).toBeNull();
    expect(visibleSuggestion({ ...base, uploadsPending: true })).toBeNull();
  });

  it('is hidden while a turn runs, for other CLIs and without a suggestion', () => {
    expect(visibleSuggestion({ ...base, activity: 'working' })).toBeNull();
    expect(visibleSuggestion({ ...base, runSlotFree: false })).toBeNull();
    expect(visibleSuggestion({ ...base, harness: 'codex' })).toBeNull();
    expect(visibleSuggestion({ ...base, harness: undefined })).toBeNull();
    expect(visibleSuggestion({ ...base, suggestion: null })).toBeNull();
    expect(visibleSuggestion({ ...base, suggestion: '   ' })).toBeNull();
  });
});

describe('acceptsWithTab', () => {
  const key = { key: 'Tab', shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, isComposing: false };
  it('accepts plain Tab only', () => {
    expect(acceptsWithTab(key)).toBe(true);
    expect(acceptsWithTab({ ...key, shiftKey: true })).toBe(false);
    expect(acceptsWithTab({ ...key, ctrlKey: true })).toBe(false);
    expect(acceptsWithTab({ ...key, isComposing: true })).toBe(false);
    expect(acceptsWithTab({ ...key, key: 'Enter' })).toBe(false);
  });
});
