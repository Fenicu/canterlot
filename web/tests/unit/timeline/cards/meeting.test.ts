import { describe, expect, it } from 'vitest';
import { resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { shortRun, taskLines, timecode, type MeetingData } from '../../../../src/lib/timeline/cards/defs/meeting';
import { made } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');
const RUN = '01a1e8fc-7528-7318-9ce6-f7d54d39a5ad';

function card(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: MeetingData }) | null;
}

export const HITS = [
  { chunk_index: 12, kind: 'transcript', start_seconds: 754.2, end_seconds: 790, speaker_labels: ['Ольга', 'Иван'], text: 'Договорились перенести релиз на четверг, если тесты пройдут.', score: 0.82 },
  { chunk_index: 40, kind: 'transcript', start_seconds: 3725, end_seconds: 3760, speaker_labels: ['Иван'], text: 'Релиз зависит от миграции лимитов.', score: 0.71 }
];

describe('meeting card', () => {
  it('search_within_run_counts_fragments_and_previews_timecoded_lines', () => {
    const resolved = card(made('mcp__meeting-pipeline__search_within_run', { run_id: RUN, query: 'релиз' }, HITS.map((hit) => JSON.stringify(hit)).join('\n')))!;
    expect(resolved.line).toBe('Встреча 01a1e8fc: поиск «релиз» · 2 фрагмента');
    expect(resolved.data.preview).toEqual([
      { lead: '12:34 · Ольга, Иван', text: 'Договорились перенести релиз на четверг, если тесты пройдут.' },
      { lead: '1:02:05 · Иван', text: 'Релиз зависит от миграции лимитов.' }
    ]);
  });

  it('search_meetings_reads_items_and_the_degraded_note', () => {
    const answer = { items: [{ id: 'r1', title: 'Планёрка', meeting_date: '2026-11-03', snippet: 'про <b>релиз</b>' }], note: 'semantic search degraded' };
    const resolved = card(made('meeting-pipeline_search_meetings', { query: 'релиз' }, JSON.stringify(answer)))!;
    expect(resolved.line).toBe('Встречи: поиск «релиз» · 1 встреча');
    expect(resolved.data.preview).toEqual([{ lead: '03.11.2026', text: 'Планёрка — про релиз' }]);
    expect(resolved.data.note).toBe('поиск без смысловой части');
  });

  it('summary_and_transcript_never_dump_the_text', () => {
    const summary = '# Планёрка 3 ноября\n\n## Решения\n- Релиз в четверг\n' + 'x'.repeat(5000);
    const resolved = card(made('mcp__meeting-pipeline__get_summary', { run_id: RUN }, summary))!;
    expect(resolved.line).toBe('Саммари встречи 01a1e8fc');
    expect(resolved.data.preview).toEqual([{ lead: '', text: 'Планёрка 3 ноября' }]);
    const transcript = card(made('mcp__meeting-pipeline__get_transcript', { run_id: RUN }, '**Ольга** [00:01]: '.repeat(3000)))!;
    expect(transcript.line).toMatch(/^Расшифровка встречи 01a1e8fc · 56\s999 знаков$/);
    expect(transcript.data.preview).toEqual([]);
  });

  it('tasks_are_counted_from_the_markdown_list', () => {
    const tasks = '# Задачи\n\n- [ ] **Иван**: подготовить миграцию\n  - подпункт\n- [x] Ольга: проверить тесты\n1. Позвонить заказчику\n';
    expect(taskLines(tasks)).toEqual(['Иван: подготовить миграцию', 'Ольга: проверить тесты', 'Позвонить заказчику']);
    const resolved = card(made('mcp__meeting-pipeline__get_tasks', { run_id: RUN }, tasks))!;
    expect(resolved.line).toBe('Задачи встречи 01a1e8fc · 3 задачи');
    expect(resolved.data.preview).toHaveLength(3);
  });

  it('list_runs_get_run_analysis_speakers_and_workspaces', () => {
    const runs = [{ id: 'a', title: 'Планёрка', meeting_date: '2026-11-03' }, { id: 'b', title: 'Ретро', created_at: '2026-11-01T10:00:00Z' }];
    expect(card(made('mcp__meeting-pipeline__list_runs', {}, JSON.stringify(runs)))!.line).toBe('Встречи: 2');
    expect(card(made('mcp__meeting-pipeline__get_run', { run_id: RUN }, JSON.stringify({ title: 'Планёрка', meeting_date: '2026-11-03', stage: 'done', tags: ['релиз'] })))!.line).toBe('Встреча «Планёрка»');
    expect(card(made('mcp__meeting-pipeline__get_analysis', { run_id: RUN }, JSON.stringify({ topics: [1, 2], decisions: [1], risks: [] })))!.data.preview).toEqual([{ lead: '', text: '2 темы, 1 решение, 0 рисков' }]);
    expect(card(made('mcp__meeting-pipeline__list_speakers', {}, JSON.stringify([{ name: 'Ольга' }, { name: 'Иван' }])))!.line).toBe('Спикеры · 2 спикера');
    expect(card(made('mcp__meeting-pipeline__list_workspaces', {}, '{"id":"w1","name":"Работа"}\n{"id":"w2","name":"Дом"}'))!.line).toBe('Пространства встреч · 2 пространства');
  });

  it('unknown_shapes_and_errors_stay_a_line', () => {
    expect(card(made('mcp__meeting-pipeline__search_within_run', { run_id: RUN, query: 'x' }, 'не json'))!.line).toBe('Встреча 01a1e8fc: поиск «x» · 0 фрагментов');
    expect(card(made('mcp__meeting-pipeline__export_everything', { run_id: RUN }, 'ok'))!.line).toBe('Встречи · export everything 01a1e8fc');
  });

  it('helpers', () => {
    expect(shortRun(RUN)).toBe('01a1e8fc');
    expect(shortRun('run-7')).toBe('run-7');
    expect(timecode(5)).toBe('0:05');
    expect(timecode(null)).toBe('');
  });
});
