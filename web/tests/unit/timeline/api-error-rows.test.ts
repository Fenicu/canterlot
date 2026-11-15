import { describe, expect, it } from 'vitest';
import { BlockModel } from '#lib/stores/blocks.js';
import type { TimelineEntry } from '#lib/stores/timeline.js';
import { RowsCache, buildRows } from '#lib/timeline/rows.js';

const NOW = Date.parse('2026-11-10T12:00:00.000Z');
const HARNESS = { id: 'claude_code', provider: 'claude' };
const VERIFY =
  "403: API Error: Your account can't receive replies from Claude right now. You can find the next step in the Anthropic Verification Portal: https://portal.anthropic.com/verify?org=01a1dfb1-10bc-7230-8b59-a74abd632567";

function entry(
  id: string,
  seq: string,
  kind: 'text' | 'warning' | 'error',
  text: string,
  run: string | null = null
): TimelineEntry {
  const block = new BlockModel(id, kind);
  block.inputCommandId = run;
  block.createdSeq = seq;
  block.createdAt = NOW;
  block.text = text;
  return { kind: 'block', key: `block:${id}`, seq, block };
}

/** Виды строк без подписи дня: она встаёт перед первой записью в любом наборе. */
function kinds(rows: ReturnType<typeof buildRows>): string[] {
  return rows.filter((row) => row.kind !== 'day').map((row) => row.kind);
}

describe('timeline/rows api errors', () => {
  it('an api error block is its own system row, not an empty answer card', () => {
    const rows = buildRows([entry('e1', '1', 'error', VERIFY, 'run-1')], { now: NOW, harness: HARNESS });
    expect(kinds(rows)).toEqual(['system']);
    expect(rows.at(-1)?.entries.map((item) => item.kind === 'block' && item.block.id)).toEqual(['e1']);
  });

  it('a run keeps the text card and puts the error after it', () => {
    const rows = buildRows(
      [
        entry('t1', '1', 'text', 'Проверил', 'run-1'),
        entry('e1', '2', 'error', '529: API Error: Overloaded', 'run-1')
      ],
      { now: NOW, harness: HARNESS }
    );
    expect(kinds(rows)).toEqual(['answer', 'system']);
  });

  it('a run after the error opens its own card', () => {
    const rows = buildRows(
      [
        entry('e1', '1', 'error', '529: API Error: Overloaded', 'run-1'),
        entry('t1', '2', 'text', 'Продолжил', 'run-1')
      ],
      { now: NOW, harness: HARNESS }
    );
    expect(kinds(rows)).toEqual(['system', 'answer']);
    expect(new Set(rows.filter((row) => row.kind !== 'day').map((row) => row.key)).size).toBe(2);
  });

  it('a non api error stays a notice inside the answer card', () => {
    const text = 'Запуск остановлен: делегированной сессии доступны чужие серверы MCP (x).';
    const rows = buildRows([entry('e1', '1', 'error', text, 'run-1')], { now: NOW, harness: HARNESS });
    expect(kinds(rows)).toEqual(['answer']);
    expect(rows.at(-1)?.entries).toHaveLength(1);
  });

  it('a warning block is never an api error bubble', () => {
    const rows = buildRows([entry('w1', '1', 'warning', VERIFY, 'run-1')], { now: NOW, harness: HARNESS });
    expect(kinds(rows)).toEqual(['answer']);
  });

  it('rows split once the streamed text proves the provider error', () => {
    const model = new BlockModel('e1', 'error');
    model.inputCommandId = 'run-1';
    model.createdSeq = '1';
    model.createdAt = NOW;
    const block: TimelineEntry = { kind: 'block', key: 'block:e1', seq: '1', block: model };
    const cache = new RowsCache();
    expect(kinds(cache.build([block], { now: NOW, harness: HARNESS }).rows)).toEqual([
      'answer'
    ]);
    model.text = VERIFY;
    expect(kinds(cache.build([block], { now: NOW, harness: HARNESS }).rows)).toEqual([
      'system'
    ]);
  });

  it('the classifier follows the harness when the text does not name the provider', () => {
    const bare = entry('e1', '1', 'error', '401: Please run /login', 'run-1');
    expect(kinds(buildRows([bare], { now: NOW, harness: HARNESS }))).toEqual(['system']);
    expect(kinds(buildRows([bare], { now: NOW }))).toEqual(['answer']);
  });

  it('a harness that arrives later rebuilds the rows instead of appending to the old ones', () => {
    const bare = entry('e1', '1', 'error', '401: Please run /login', 'run-1');
    const tail = entry('t1', '2', 'text', 'Продолжил', 'run-2');
    const cache = new RowsCache();
    expect(kinds(cache.build([bare], { now: NOW }).rows)).toEqual(['answer']);
    expect(kinds(cache.build([bare, tail], { now: NOW, harness: HARNESS }).rows)).toEqual([
      'system',
      'answer'
    ]);
  });
});
