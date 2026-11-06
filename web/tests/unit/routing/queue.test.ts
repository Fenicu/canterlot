import { expect, test } from 'vitest';
import { itemTarget, itemTitle, pendingItems, queueSummary, stageIndex } from '#lib/routing/queue.js';

const titles = (key: string) => (key === 'bugfix' ? 'Исправление' : key);

test('queue items are named by the requested title or a fallback', () => {
  expect(itemTitle({ title: 'Фикс гонки' })).toBe('Фикс гонки');
  expect(itemTitle({ title: null })).toBe('Запуск без названия');
  expect(itemTitle({ title: '  ' })).toBe('Запуск без названия');
});

test('routed items show the task type and level, direct ones the pair', () => {
  expect(itemTarget({ task_type: 'bugfix', level: 'high' }, titles)).toBe('маршрут «Исправление» · высокое');
  expect(itemTarget({ task_type: 'review', level: null }, titles)).toBe('маршрут «review»');
  expect(itemTarget({ harness: 'codex', model: 'gpt-6.1-sol' }, titles, () => 'GPT-6.1 Sol')).toBe('GPT-6.1 Sol');
  expect(itemTarget({ harness: 'codex', model: 'default' }, titles)).toBe('Codex');
  expect(itemTarget({}, titles)).toBe('');
});

test('summary counts waiting items and names the nearest start', () => {
  const base = { reason: 'rate', expires_at: '2026-11-07T00:00:00.000Z', created_at: '2026-11-06T00:00:00.000Z' };
  const now = Date.parse('2026-11-06T10:00:00.000Z');
  const items = [
    { ...base, id: 'a', state: 'queued', not_before: '2026-11-06T09:00:00.000Z' },
    { ...base, id: 'b', state: 'cancelled', not_before: '2026-11-06T08:00:00.000Z' }
  ];
  expect(pendingItems(items).map((item) => item.id)).toEqual(['a']);
  expect(queueSummary(items, now)).toBe('В очереди запуска: 1 · следующий сейчас');
  expect(queueSummary([], now)).toBe('');
  expect(stageIndex({ ...items[0]!, state: 'starting' })).toBe(1);
  expect(stageIndex({ ...items[0]!, state: 'started' })).toBe(2);
});
