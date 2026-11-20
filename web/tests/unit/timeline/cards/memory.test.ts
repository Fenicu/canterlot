import { describe, expect, it } from 'vitest';
import { cardCall } from '../../../../src/lib/timeline/cards/call';
import type { MemoryDeleteData, MemoryGetData, MemorySearchData, MemoryUpdateData, MemoryWriteData } from '../../../../src/lib/timeline/cards/defs/memory';
import { kindWord, memoryHref } from '../../../../src/lib/timeline/cards/defs/memory';
import { resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { callBlock, hub, made, resultBlock } from './helpers';

const NOW = Date.parse('2026-11-20T10:00:00Z');
const ID = '01a1b3b9-0000-7000-8000-00000000bb01';
const OTHER = '01a1b3b9-0000-7000-8000-00000000bb02';

function card<D>(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: D }) | null;
}

function failed(tool: string, input: unknown, output: string, root = true) {
  return cardCall(callBlock(tool, input), resultBlock(output, { summary: 'error' }), { root });
}

const row = (patch: Record<string, unknown> = {}) => ({ id: ID, kind: 'gotcha', level: 'project', project_label: 'canterlot', subject: 'Миграции в тестовой базе', excerpt: 'Порт 55883', updated_at: '2026-11-18T09:00:00Z', expired: false, ...patch });

describe('kind labels and links', () => {
  it('names_every_kind_in_short_russian', () => {
    expect(['user', 'feedback', 'project', 'reference', 'decision', 'gotcha', 'environment', 'incident'].map(kindWord)).toEqual(['владелец', 'урок', 'проект', 'справка', 'решение', 'грабли', 'окружение', 'инцидент']);
    expect(kindWord('weird')).toBe('weird');
  });

  it('links_to_the_memory_screen_with_the_entry_and_an_optional_domain', () => {
    expect(memoryHref(ID)).toBe(`/settings?c=memory&entry=${ID}`);
    expect(memoryHref(ID, 'personal')).toBe(`/settings?c=memory&entry=${ID}&domain=personal`);
    expect(memoryHref(ID, 'strange')).toBe(`/settings?c=memory&entry=${ID}`);
  });
});

describe('memory_search', () => {
  const input = { query: 'тестовая база' };

  it('speaks_in_both_voices_and_counts_the_rows', () => {
    const answer = hub({ count: 2, results: [row(), row({ id: OTHER, kind: 'decision', level: 'domain', project_label: null, subject: 'Порты', expired: true })] });
    const she = card<MemorySearchData>(made('mcp__canterlot__memory_search', input, answer))!;
    expect(she.line).toBe('Искала в памяти: «тестовая база»');
    expect(she.word).toEqual({ word: '2 записи', tone: 'muted' });
    expect(she.data.results.map((item) => [item.kind, item.level, item.projectLabel, item.expired])).toEqual([
      ['gotcha', 'project', 'canterlot', false],
      ['decision', 'domain', '', true]
    ]);
    const neutral = card<MemorySearchData>(cardCall(callBlock('canterlot_memory_search', input), resultBlock(answer), { root: false }))!;
    expect(neutral.line).toBe('Поиск в памяти: «тестовая база»');
  });

  it('an_empty_query_reads_as_new_entries_and_no_rows_as_nothing', () => {
    const resolved = card<MemorySearchData>(made('mcp__canterlot__memory_search', {}, hub({ count: 0, results: [] })))!;
    expect(resolved.line).toBe('Новые записи памяти');
    expect(resolved.word).toEqual({ word: 'ничего', tone: 'muted' });
    expect(card<MemorySearchData>(made('canterlot.memory_search', { query: '   ' }, hub({ count: 1, results: [row()] })))!.line).toBe('Новые записи памяти');
  });

  it('agrees_the_count_with_plurals', () => {
    const one = card<MemorySearchData>(made('canterlot.memory_search', input, hub({ count: 1, results: [row()] })))!;
    expect(one.word?.word).toBe('1 запись');
    const five = card<MemorySearchData>(made('canterlot.memory_search', input, hub({ count: 5, results: Array.from({ length: 5 }, (_, i) => row({ id: `${ID}-${i}` })) })))!;
    expect(five.word?.word).toBe('5 записей');
  });

  it('keeps_a_row_with_a_missing_field_and_drops_one_without_an_id', () => {
    const resolved = card<MemorySearchData>(made('canterlot.memory_search', input, hub({ count: 2, results: [{ id: ID, subject: 'Без вида' }, { subject: 'Без id' }] })))!;
    expect(resolved.data.results).toHaveLength(1);
    expect(resolved.data.results[0]).toMatchObject({ id: ID, subject: 'Без вида', kind: '', expired: false, updatedAt: null });
  });

  it('a_running_call_names_the_query_and_waits', () => {
    const resolved = card<MemorySearchData>(made('canterlot.memory_search', input, null, { working: true }))!;
    expect(resolved.line).toBe('Поиск в памяти: «тестовая база»');
    expect(resolved.word).toMatchObject({ word: 'идёт' });
  });

  it('a_refusal_reads_in_words', () => {
    const resolved = card<MemorySearchData>(failed('mcp__canterlot__memory_search', { query: 'x', kind: 'nope' }, 'invalid: Invalid memory field {"field":"kind"}'))!;
    expect(resolved.line).toBe('Отказ: неверное поле kind');
    expect(resolved.word).toMatchObject({ word: 'ошибка', tone: 'danger' });
  });

  it('without_an_answer_the_feed_shows_the_plain_block', () => {
    expect(card(made('mcp__canterlot__memory_search', input, ''))).toBeNull();
    expect(card(made('mcp__canterlot__memory_search', input, 'not json at all'))).toBeNull();
  });
});

describe('memory_get', () => {
  const full = { id: ID, domain: 'work', level: 'project', project_label: 'canterlot', kind: 'gotcha', subject: 'Миграции в тестовой базе', body: '**Порт** 55883', status: 'active', valid_until: '2026-12-01T00:00:00Z', expired: false, version: 4, created_by: { kind: 'agent' }, updated_by: { kind: 'owner' } };

  it('reads_the_entry_with_kind_level_version_and_expiry', () => {
    const resolved = card<MemoryGetData>(made('mcp__canterlot__memory_get', { id: ID }, hub(full)))!;
    expect(resolved.line).toBe('Прочитала запись: Миграции в тестовой базе');
    expect(resolved.link).toEqual({ text: 'Миграции в тестовой базе', href: `/settings?c=memory&entry=${ID}&domain=work` });
    expect(resolved.data).toMatchObject({ id: ID, kind: 'gotcha', level: 'project', projectLabel: 'canterlot', version: 4, body: '**Порт** 55883' });
    expect(resolved.data.validUntil).toBe(Date.parse('2026-12-01T00:00:00Z'));
    expect(card<MemoryGetData>(cardCall(callBlock('canterlot_memory_get', { id: ID }), resultBlock(hub(full)), { root: false }))!.line).toBe('Чтение записи: Миграции в тестовой базе');
  });

  it('no_expiry_leaves_the_field_empty', () => {
    const resolved = card<MemoryGetData>(made('canterlot.memory_get', { id: ID }, hub({ ...full, valid_until: null })))!;
    expect(resolved.data.validUntil).toBeNull();
  });

  it('not_found_is_a_refusal', () => {
    const resolved = card<MemoryGetData>(failed('mcp__canterlot__memory_get', { id: ID }, 'not_found: No such memory entry'))!;
    expect(resolved.line).toBe('Отказ: записи нет');
    expect(resolved.word).toMatchObject({ tone: 'danger' });
  });

  it('a_running_call_without_input_or_answer_stays_plain', () => {
    expect(card(made('mcp__canterlot__memory_get', '{', null, { working: true }))).toBeNull();
    expect(card<MemoryGetData>(made('mcp__canterlot__memory_get', { id: ID }, null, { working: true }))!.line).toBe('Чтение записи');
  });

  it('an_answer_without_a_subject_is_plain', () => {
    expect(card(made('mcp__canterlot__memory_get', { id: ID }, hub({ id: ID })))).toBeNull();
  });
});

describe('memory_write', () => {
  const input = { kind: 'feedback', subject: 'Не писать эмодзи', body: 'Владелец не любит эмодзи в карточках.', level: 'domain' };
  const answer = { id: ID, domain: 'personal', level: 'domain', project_label: null, version: 1 };

  it('remembers_with_kind_and_level', () => {
    const resolved = card<MemoryWriteData>(made('mcp__canterlot__memory_write', input, hub(answer)))!;
    expect(resolved.line).toBe('Запомнила: Не писать эмодзи');
    expect(resolved.link).toEqual({ text: 'Не писать эмодзи', href: `/settings?c=memory&entry=${ID}&domain=personal` });
    expect(resolved.data).toMatchObject({ kind: 'feedback', level: 'domain', body: input.body, id: ID, refusal: null });
    expect(resolved.word).toBeNull();
    expect(card<MemoryWriteData>(cardCall(callBlock('canterlot_memory_write', input), resultBlock(hub(answer)), { root: false }))!.line).toBe('Запись в память: Не писать эмодзи');
  });

  it('level_defaults_to_the_project_the_answer_names', () => {
    const resolved = card<MemoryWriteData>(made('canterlot.memory_write', { kind: 'gotcha', subject: 'S', body: 'B' }, hub({ ...answer, level: 'project', project_label: 'canterlot' })))!;
    expect(resolved.data).toMatchObject({ level: 'project', projectLabel: 'canterlot' });
  });

  it('a_secret_refusal_says_so', () => {
    const resolved = card<MemoryWriteData>(failed('mcp__canterlot__memory_write', input, 'looks_like_secret: The text looks like a secret {"class":"token_prefix"}'))!;
    expect(resolved.line).toBe('Отказ: похоже на секрет');
    expect(resolved.word).toMatchObject({ word: 'ошибка', tone: 'danger' });
    expect(resolved.data.refusal).toMatchObject({ code: 'looks_like_secret', secretClass: 'token_prefix' });
  });

  it('an_existing_subject_links_the_existing_entry', () => {
    const resolved = card<MemoryWriteData>(failed('mcp__canterlot__memory_write', input, `subject_exists: An active entry with this subject already exists {"id":"${OTHER}","version":3}`))!;
    expect(resolved.line).toBe('Уже есть запись с такой темой');
    expect(resolved.link).toEqual({ text: 'запись', href: `/settings?c=memory&entry=${OTHER}` });
    expect(resolved.data.refusal).toMatchObject({ code: 'subject_exists', id: OTHER, version: 3 });
  });

  it('an_existing_subject_without_an_id_has_no_link', () => {
    const resolved = card<MemoryWriteData>(failed('mcp__canterlot__memory_write', input, 'subject_exists: An active entry with this subject already exists'))!;
    expect(resolved.line).toBe('Уже есть запись с такой темой');
    expect(resolved.link).toBeNull();
  });

  it.each([
    ['rate_limited: Too many requests {"retry_after_s":600}', 'Лимит записей в час'],
    ['too_large: The payload is too large {"limit":8192}', 'Отказ: запись слишком большая'],
    ['domain_full: The memory of this domain is full {"limit":500}', 'Отказ: память домена заполнена'],
    ['bad_level: This session has no project, so project level is not possible', 'Отказ: у сессии нет проекта, запись только на весь домен'],
    ['memory_unavailable: This session has no memory', 'Отказ: у этой сессии нет памяти'],
    ['something_new: Strange', 'Отказ: something_new']
  ])('refusal_%#_reads_%s', (output, line) => {
    expect(card<MemoryWriteData>(failed('mcp__canterlot__memory_write', input, output))!.line).toBe(line);
  });

  it('a_failure_without_a_code_names_the_call_not_the_code', () => {
    expect(card<MemoryWriteData>(failed('mcp__canterlot__memory_write', input, 'Tool call failed'))!.line).toBe('Отказ: вызов не удался');
  });

  it('partial_input_still_makes_a_card_and_an_empty_one_does_not', () => {
    const partial = cardCall(callBlock('mcp__canterlot__memory_write', '{"kind":"feedback","subject":"Не писать'), null, { root: true, working: true });
    expect(card<MemoryWriteData>(partial)?.line ?? '').toContain('Не писать');
    expect(card(made('mcp__canterlot__memory_write', {}, null))).toBeNull();
  });
});

describe('memory_update', () => {
  const answer = { id: ID, domain: 'work', level: 'project', project_label: 'canterlot', version: 5 };

  it('names_the_subject_from_an_earlier_lookup_and_the_version_move', () => {
    const call = made('mcp__canterlot__memory_update', { id: ID, expected_version: 4, body: 'Новый текст' }, hub(answer), { memorySubject: (id) => (id === ID ? 'Миграции в тестовой базе' : null) });
    const resolved = card<MemoryUpdateData>(call)!;
    expect(resolved.line).toBe('Обновила запись: Миграции в тестовой базе');
    expect(resolved.word).toEqual({ word: 'версия 4 → 5', tone: 'muted' });
    expect(resolved.data.changes).toEqual(['text']);
    expect(resolved.data.body).toBe('Новый текст');
    expect(card<MemoryUpdateData>(cardCall(callBlock('canterlot_memory_update', { id: ID, expected_version: 4, body: 'x' }), resultBlock(hub(answer)), { root: false }))!.line).toBe(`Обновление записи: ${ID.slice(0, 8)}`);
  });

  it('falls_back_to_the_new_subject_then_to_the_short_id', () => {
    const named = card<MemoryUpdateData>(made('canterlot.memory_update', { id: ID, expected_version: 1, subject: 'Новая тема' }, hub({ ...answer, version: 2 })))!;
    expect(named.line).toBe('Обновила запись: Новая тема');
    const bare = card<MemoryUpdateData>(made('canterlot.memory_update', { id: ID, expected_version: 1, kind: 'decision' }, hub({ ...answer, version: 2 })))!;
    expect(bare.line).toBe(`Обновила запись: ${ID.slice(0, 8)}`);
  });

  it('lists_every_changed_field_in_screen_order', () => {
    const input = { id: ID, expected_version: 2, subject: 'T', kind: 'decision', level: 'domain', valid_until: null, body: 'B' };
    const resolved = card<MemoryUpdateData>(made('canterlot.memory_update', input, hub({ ...answer, version: 3 })))!;
    expect(resolved.data.changes).toEqual(['subject', 'kind', 'level', 'expiry', 'text']);
    expect(resolved.data.expiryCleared).toBe(true);
    expect(card<MemoryUpdateData>(made('canterlot.memory_update', { id: ID, expected_version: 2, valid_until: '2026-12-01T00:00:00Z' }, hub({ ...answer, version: 3 })))!.data).toMatchObject({ changes: ['expiry'], expiryCleared: false, validUntil: Date.parse('2026-12-01T00:00:00Z') });
  });

  it('a_version_conflict_names_the_current_version', () => {
    const resolved = card<MemoryUpdateData>(failed('mcp__canterlot__memory_update', { id: ID, expected_version: 4, body: 'x' }, 'version_conflict: The entry changed since you read it {"current_version":6}'))!;
    expect(resolved.line).toBe('Отказ: запись уже изменили (версия 6)');
    expect(resolved.word).toMatchObject({ tone: 'danger' });
  });

  it('a_conflict_without_a_version_still_reads', () => {
    expect(card<MemoryUpdateData>(failed('mcp__canterlot__memory_update', { id: ID, expected_version: 4 }, 'version_conflict: The entry changed since you read it'))!.line).toBe('Отказ: запись уже изменили');
  });

  it('a_secret_in_the_new_text_is_refused', () => {
    expect(card<MemoryUpdateData>(failed('mcp__canterlot__memory_update', { id: ID, expected_version: 4, body: 'x' }, 'looks_like_secret: The text looks like a secret {"class":"assignment"}'))!.line).toBe('Отказ: похоже на секрет');
  });

  it('without_an_id_the_feed_shows_the_plain_block', () => {
    expect(card(made('mcp__canterlot__memory_update', { body: 'x' }, hub(answer)))).toBeNull();
  });
});

describe('memory_delete', () => {
  it('archives_and_names_the_entry_from_an_earlier_lookup', () => {
    const call = made('mcp__canterlot__memory_delete', { id: ID, expected_version: 4 }, hub({ id: ID, status: 'archived', version: 5 }), { memorySubject: () => 'Миграции в тестовой базе' });
    const resolved = card<MemoryDeleteData>(call)!;
    expect(resolved.line).toBe('Убрала в архив: Миграции в тестовой базе');
    expect(resolved.word).toBeNull();
    expect(card<MemoryDeleteData>(cardCall(callBlock('canterlot_memory_delete', { id: ID, expected_version: 4 }), resultBlock(hub({ id: ID, status: 'archived', version: 5 })), { root: false }))!.line).toBe(`Архивация записи: ${ID.slice(0, 8)}`);
  });

  it('without_a_lookup_it_names_the_short_id', () => {
    expect(card<MemoryDeleteData>(made('canterlot.memory_delete', { id: ID, expected_version: 4 }, hub({ id: ID, status: 'archived', version: 5 })))!.line).toBe(`Убрала в архив: ${ID.slice(0, 8)}`);
  });

  it('a_conflict_and_a_missing_entry_are_refusals', () => {
    expect(card<MemoryDeleteData>(failed('mcp__canterlot__memory_delete', { id: ID, expected_version: 1 }, 'version_conflict: x {"current_version":2}'))!.line).toBe('Отказ: запись уже изменили (версия 2)');
    expect(card<MemoryDeleteData>(failed('mcp__canterlot__memory_delete', { id: ID, expected_version: 1 }, 'not_found: No such memory entry'))!.line).toBe('Отказ: записи нет');
  });

  it('without_an_id_the_feed_shows_the_plain_block', () => {
    expect(card(made('mcp__canterlot__memory_delete', {}, hub({ status: 'archived' })))).toBeNull();
  });
});
