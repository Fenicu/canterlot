import { describe, expect, it } from 'vitest';
import { resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { toolId } from '../../../../src/lib/timeline/cards/toolId';
import { ciDuration, type ForgejoData } from '../../../../src/lib/timeline/cards/defs/forgejo';
import { jsonItems, jsonSequence } from '../../../../src/lib/timeline/cards/json';
import { made } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');

function card(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: ForgejoData }) | null;
}

export const RUNS = {
  total_count: 3,
  workflow_runs: [
    { id: 901, index_in_repo: 128, title: 'Сделать карточки ленты', workflow_id: 'ci.yml', status: 'failure', prettyref: 'main', duration: 252_000_000_000, html_url: 'https://forgejo.example/owner/canterlot/actions/runs/128' },
    { id: 902, index_in_repo: 129, title: 'Пузыри', workflow_id: '.forgejo/workflows/release.yaml', status: 'running', prettyref: 'canterlot/4d39a5ad', started: '2026-11-07T10:00:00Z' },
    { id: 903, index_in_repo: 127, title: 'Документация', workflow_id: 'ci.yml', status: 'success', prettyref: 'main', started: '2026-11-07T09:00:00Z', stopped: '2026-11-07T09:03:30Z' }
  ]
};

describe('tool ids', () => {
  it('forgejo_and_meeting_pipeline_are_families_in_every_harness', () => {
    expect(toolId('mcp__forgejo__list_workflow_runs')).toBe('forgejo.list_workflow_runs');
    expect(toolId('forgejo_list_action_run_jobs')).toBe('forgejo.list_action_run_jobs');
    expect(toolId('forgejo.list_workflow_runs')).toBe('forgejo.list_workflow_runs');
    expect(toolId('mcp__meeting-pipeline__search_within_run')).toBe('meeting.search_within_run');
    expect(toolId('meeting-pipeline_get_summary')).toBe('meeting.get_summary');
    expect(toolId('meeting_pipeline_get_tasks')).toBe('meeting.get_tasks');
    expect(toolId('meeting-pipeline.list_runs')).toBe('meeting.list_runs');
  });
});

describe('json sequences', () => {
  it('reads_arrays_wrapped_results_and_objects_one_per_block', () => {
    expect(jsonItems('[{"a":1},{"a":2}]')).toHaveLength(2);
    expect(jsonItems('{"result":[{"a":1}]}')).toHaveLength(1);
    expect(jsonItems('{"a":1}\n{"a":2}\n{"a":3}')).toHaveLength(3);
    expect(jsonItems('{\n  "a": "}"\n}\n{\n  "a": 2\n}')).toHaveLength(2);
    expect(jsonItems('{"workflow_runs":[{"id":1}],"total_count":1}', ['workflow_runs'])).toEqual([{ id: 1 }]);
    expect(jsonSequence('not json')).toEqual([]);
  });
});

describe('forgejo card', () => {
  it('workflow_runs_list_status_name_ref_duration_and_link', () => {
    const resolved = card(made('mcp__forgejo__list_workflow_runs', { owner: 'owner', repo: 'canterlot', limit: 3 }, JSON.stringify(RUNS)))!;
    expect(resolved.line).toBe('CI owner/canterlot · запуски: 3 · 1 сбой · 1 идёт');
    expect(resolved.data.rows.map((row) => [row.status, row.name, row.number, row.ref, row.took])).toEqual([
      ['failure', 'ci', '#128', 'main', '4 мин'],
      ['running', 'release', '#129', 'canterlot/4d39a5ad', ''],
      ['success', 'ci', '#127', 'main', '3 мин']
    ]);
    expect(resolved.data.rows[0]!.url).toBe('https://forgejo.example/owner/canterlot/actions/runs/128');
  });

  it('run_jobs_list_their_names_and_states', () => {
    const answer = { jobs: [{ id: 1, name: 'test', status: 'success', runs_on: ['docker'] }, { id: 2, name: 'e2e', status: 'cancelled' }] };
    const resolved = card(made('mcp__forgejo__list_action_run_jobs', { owner: 'owner', repo: 'canterlot', run_id: 901 }, JSON.stringify(answer)))!;
    expect(resolved.line).toBe('CI owner/canterlot · задания запуска 901: 2');
    expect(resolved.data.rows.map((row) => [row.status, row.name, row.ref])).toEqual([['success', 'test', 'docker'], ['cancelled', 'e2e', '']]);
  });

  it('unknown_forgejo_tools_and_unreadable_answers_stay_a_compact_line', () => {
    expect(card(made('mcp__forgejo__get_issue_by_index', { owner: 'o', repo: 'r', index: 4 }, '{"title":"x"}'))!.line).toBe('Forgejo · get issue by index o/r');
    const garbage = card(made('mcp__forgejo__list_workflow_runs', { owner: 'o', repo: 'r' }, 'API call completed'))!;
    expect(garbage.data.rows).toEqual([]);
    expect(garbage.line).toBe('CI o/r · запуски');
  });

  it('duration_reads_nanoseconds_or_timestamps', () => {
    expect(ciDuration({ duration: 90_000_000_000 })).toBe('1 мин');
    expect(ciDuration({ started: '2026-11-07T09:00:00Z', stopped: '2026-11-07T09:00:42Z' })).toBe('42 с');
    expect(ciDuration({})).toBe('');
  });
});
