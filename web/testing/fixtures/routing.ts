// Маршрутизация и очередь запусков для e2e, экранов visual-language и снимков на ревью.
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation, seedMachine } from '../fake-hub/seed';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';

export const DEEPSEEK = 'opencode-go/deepseek-v4.1-flash';

export function seedRoutingCatalog(api: FakeHubApi) {
  return seedMachine(api, {
    harnesses: [
      { id: 'opencode', title: 'OpenCode', provider: 'opencode', models: [{ id: DEEPSEEK, title: 'DeepSeek', efforts: ['low', 'medium', 'high'] }, { id: 'opencode-go/beam-free', title: 'Beam', efforts: [] }], permission_modes: ['owner'] },
      { id: 'claude_code', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus[1m]', title: 'Opus 1M', efforts: ['high'] }], permission_modes: ['owner'] },
      { id: 'codex', title: 'Codex', provider: 'codex', models: [{ id: 'gpt-6.1-sol', title: 'GPT-6.1 Sol', efforts: ['high'] }], permission_modes: ['owner'] }
    ]
  });
}

export function seedRoutingWhitelist(api: FakeHubApi): void {
  api.spawnWhitelist = [
    { harness: 'opencode', model: DEEPSEEK },
    { harness: 'codex', model: 'gpt-6.1-sol' }
  ];
}

export const ROUTE_DECISION = {
  policy_revision: 0,
  task_type: 'bugfix',
  level: null,
  harness: 'opencode',
  model: DEEPSEEK,
  effort: 'high',
  preset: 'Исполнитель (английский)',
  nudge: { max_nudges: 2 },
  watchdog_minutes: 90,
  considered: [
    { harness: 'codex', model: 'gpt-6.1-sol', verdict: 'limits_over', reason: 'codex weekly 83% > 80%' },
    { harness: 'opencode', model: DEEPSEEK, verdict: 'chosen', reason: '' }
  ]
};

export function historyBlock(api: FakeHubApi, seq: string, partial: Partial<HistoryBlock>): HistoryBlock {
  const id = partial.id ?? api.id();
  const content = partial.content ?? '';
  const at = new Date(Date.now() - 60_000).toISOString();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return { kind: 'block', id, block_kind: 'text', content, content_bytes: content.length, content_truncated: false, created_seq: seq, created_at: at, rev: 1, state: 'final', agent: 'claude', model: 'opus-5.5', tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1, started_at: at, finished_at: at, ...partial } as HistoryBlock;
}

/** Делегат, выбранный маршрутом, и лента родителя: запуск по маршруту и запуск в очередь. */
export function seedRoutedSpawn(api: FakeHubApi, parent: string): string {
  const child = seedConversation(api, { title: 'Фикс по маршруту', parent_id: parent, spawn_depth: 1, routing: ROUTE_DECISION });
  api.details.set(child, { ...api.details.get(child)!, routing: ROUTE_DECISION });
  const run = api.id();
  const call = api.id();
  const queued = api.id();
  api.histories.set(parent, [
    historyBlock(api, '1', { id: call, block_kind: 'tool_call', tool_name: 'mcp__canterlot__session_spawn', summary: 'session_spawn', input_command_id: run, agent: 'orchestrator' }),
    historyBlock(api, '2', { block_kind: 'tool_result', parent_block_id: call, input_command_id: run, agent: 'orchestrator', content: JSON.stringify({ status: 'started', session_id: child, machine_metrics: 'stale', routing: { task_type: 'bugfix', harness: 'opencode', model: DEEPSEEK, effort: 'high', policy_revision: 0 } }, null, 2) }),
    historyBlock(api, '3', { id: queued, block_kind: 'tool_call', tool_name: 'mcp__canterlot__session_spawn', summary: 'session_spawn', input_command_id: run, agent: 'orchestrator' }),
    historyBlock(api, '4', { block_kind: 'tool_result', parent_block_id: queued, input_command_id: run, agent: 'orchestrator', content: JSON.stringify({ status: 'queued', queue_id: api.id(), reason: 'rate', detail: '10 spawns in the last hour; the next slot opens at 14:32 UTC' }) })
  ]);
  return child;
}

/** Два ожидающих запуска: частота и занятые исполнители. */
export function seedQueue(api: FakeHubApi, parent: string) {
  const first = api.queueItem(parent, { title: 'Исправить гонку подписок', task_type: 'bugfix', level: 'high', reason: 'rate', detail: { detail: '10 spawns in the last hour; the next slot opens at 14:32 UTC' }, not_before: new Date(Date.now() + 10 * 60_000).toISOString(), created_at: '2026-11-06T10:00:00.000Z' });
  const second = api.queueItem(parent, { title: null, harness: 'codex', model: 'gpt-6.1-sol', reason: 'resources', detail: { detail: 'disk 12 GB free < 30 GB. T3 idle 3 h: session_accept frees their worktrees and target dirs' }, not_before: new Date(Date.now() + 20 * 60_000).toISOString(), created_at: '2026-11-06T10:01:00.000Z', position: 2 });
  return { first, second };
}

/** Пропуск по прогнозу: за ожидаемое время задачи недельный расход перейдёт порог. */
export const PROJECTED_SKIP = { verdict: 'projected_over_threshold', reason: 'weekly 72% now, projected 84% in 60 min > 80%' } as const;
