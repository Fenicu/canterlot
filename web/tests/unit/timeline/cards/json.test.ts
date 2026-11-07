import { describe, expect, it } from 'vitest';
import { jsonObjectIn, parseJson } from '../../../../src/lib/timeline/cards/json';

describe('parseJson', () => {
  it('reads_whole_json_as_is', () => {
    expect(parseJson('{"a":1,"b":[true,null]}')).toEqual({ value: { a: 1, b: [true, null] }, partial: false });
  });

  it('keeps_the_fields_that_arrived_before_the_cut', () => {
    const text = JSON.stringify({ title: 'Фикс', isolated: true, brief: 'Первая строка\nВторая строка и дальше длинный текст' }).slice(0, 70);
    const parsed = parseJson(text);
    expect(parsed.partial).toBe(true);
    expect(parsed.value).toMatchObject({ title: 'Фикс', isolated: true });
    expect(String((parsed.value as { brief: string }).brief).startsWith('Первая строка')).toBe(true);
  });

  it('keeps_whole_items_of_a_cut_list_and_drops_a_cut_string', () => {
    const text = '{"todos":[{"content":"Один","status":"completed"},{"content":"Два","status":"in_pro';
    const parsed = parseJson(text);
    expect(parsed.partial).toBe(true);
    expect((parsed.value as { todos: unknown[] }).todos[0]).toEqual({ content: 'Один', status: 'completed' });
    expect((parsed.value as { todos: { content: string }[] }).todos[1]?.content).toBe('Два');
  });

  it('a_number_at_the_very_end_is_not_trusted', () => {
    const parsed = parseJson('{"name":"x","in_seconds":12');
    expect(parsed.value).toEqual({ name: 'x' });
  });

  it('plain_text_and_garbage_are_not_json', () => {
    expect(parseJson('sleep 90').value).toBeUndefined();
    expect(parseJson('{"a":1} trailing words').value).toBeUndefined();
    expect(parseJson('').value).toBeUndefined();
  });

  it('finds_the_hub_answer_before_a_hint_line', () => {
    const text = '{\n  "status": "started",\n  "session_id": "x"\n}\nSession created. You are subscribed.';
    expect(jsonObjectIn(text)).toEqual({ status: 'started', session_id: 'x' });
    expect(jsonObjectIn('Claude API budget: five-hour window 15% used')).toBeNull();
  });
});
