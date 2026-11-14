import { describe, expect, it } from 'vitest';
import {
  WAIT_WARN_MS,
  permissionToolName,
  questionOptionCount,
  waitFactsFromActivity,
  waitFactsFromRequest,
  waitHeadText,
  waitIsLong,
  waitStatusText
} from '#lib/requests/waitCopy.js';

const NOW = Date.parse('2026-11-14T12:00:00.000Z');
const ago = (ms: number) => new Date(NOW - ms).toISOString();

describe('requests/waitCopy', () => {
  it('says_permission_tool_and_age_in_one_short_line', () => {
    const facts = waitFactsFromRequest('permission', { tool_name: 'Bash', command: 'cargo test' }, ago(12 * 60_000));
    expect(waitStatusText(facts, NOW)).toBe('Ждёт разрешения · Bash · 12 мин');
  });

  it('counts_question_variants_with_russian_plurals', () => {
    const question = (options: number) =>
      waitStatusText(
        waitFactsFromRequest('question', { questions: [{ text: 'Какой порог?', options: Array.from({ length: options }, (_, index) => `Вариант ${index}`) }] }, ago(3 * 60_000)),
        NOW
      );
    expect(question(1)).toBe('Вопрос · 1 вариант · 3 мин');
    expect(question(2)).toBe('Вопрос · 2 варианта · 3 мин');
    expect(question(5)).toBe('Вопрос · 5 вариантов · 3 мин');
  });

  it('names_a_url_elicitation_separately', () => {
    const facts = waitFactsFromRequest('permission', { tool_name: 'mcp_elicitation', mode: 'url', url: 'https://example.com' }, ago(65 * 60_000));
    expect(facts.kind).toBe('elicitation');
    expect(waitStatusText(facts, NOW)).toBe('Ждёт ссылку · 1 ч 5 мин');
  });

  it('omits_the_tool_and_variant_count_when_the_hub_did_not_send_them', () => {
    expect(waitStatusText(waitFactsFromRequest('permission', {}, ago(60_000)), NOW)).toBe('Ждёт разрешения · 1 мин');
    expect(waitStatusText(waitFactsFromRequest('question', {}, ago(60_000)), NOW)).toBe('Вопрос · 1 мин');
  });

  it('card_head_names_the_kind_and_the_wait_in_words', () => {
    expect(waitHeadText(waitFactsFromRequest('permission', { tool_name: 'Bash' }, ago(12 * 60_000)), NOW)).toBe('Запрос разрешения · 12 мин назад');
    expect(waitHeadText(waitFactsFromRequest('question', {}, ago(3 * 60_000)), NOW)).toBe('Вопрос · 3 мин назад');
    const noAge = waitFactsFromRequest('question', {}, null);
    expect(waitHeadText(noAge, NOW)).toBe('Вопрос');
  });

  it('turns_warning_only_after_ten_minutes_of_waiting', () => {
    const at = (ms: number) => waitIsLong(waitFactsFromRequest('question', {}, ago(ms)), NOW);
    expect(at(WAIT_WARN_MS)).toBe(false);
    expect(at(WAIT_WARN_MS + 1)).toBe(true);
    expect(waitIsLong(waitFactsFromRequest('question', {}, null), NOW)).toBe(false);
  });

  it('reads_tool_and_questions_from_both_payload_shapes', () => {
    expect(permissionToolName({ tool_name: 'Bash' })).toBe('Bash');
    expect(permissionToolName({ request_permission: { tool_name: 'Edit' } })).toBe('Edit');
    expect(questionOptionCount({ questions: [{ options: ['a', 'b', 'c'] }] })).toBe(3);
    expect(questionOptionCount({ ask_question: { questions: [{ options: ['a'] }] } })).toBe(1);
    expect(questionOptionCount({ question: 'Какой порог?', options: ['a', 'b'] })).toBe(2);
    expect(questionOptionCount({ question: 'Какой порог?' })).toBeNull();
  });

  it('list_activity_derives_kind_from_phase_and_summary_when_the_hub_has_no_request_kind', () => {
    const at = (evidence: Record<string, unknown>) =>
      waitFactsFromActivity({ state: 'waiting_for_user', evidence, since: ago(12 * 60_000) });

    const permission = at({ phase: 'awaiting_permission', question: 'Bash: cargo test' });
    expect(permission?.kind).toBe('permission');
    expect(waitStatusText(permission!, NOW)).toBe('Ждёт разрешения · Bash · 12 мин');

    const question = at({ phase: 'awaiting_answer', question: 'Какой порог?' });
    expect(question?.kind).toBe('question');
    expect(waitStatusText(question!, NOW)).toBe('Вопрос · 12 мин');

    // Codex присылает вопрос с фазой разрешения: текст без имени инструмента — вопрос.
    expect(at({ phase: 'awaiting_permission', question: 'Какой порог?' })?.kind).toBe('question');

    const explicit = waitFactsFromActivity({
      state: 'waiting_for_user',
      evidence: { phase: 'awaiting_permission', request_kind: 'question', option_count: 4, request_created_at: ago(3 * 60_000) },
      since: ago(12 * 60_000)
    });
    expect(waitStatusText(explicit!, NOW)).toBe('Вопрос · 4 варианта · 3 мин');
  });

  it('returns_no_facts_for_other_states', () => {
    expect(waitFactsFromActivity({ state: 'working', evidence: { phase: 'awaiting_answer' } })).toBeNull();
  });
});
