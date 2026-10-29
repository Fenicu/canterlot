import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';

const AT = '2026-10-28T12:00:00.000Z';

export function seedMockupChat(api: FakeHubApi): string {
  const id = seedConversation(api, { title: 'Разбор ленты' });
  const run = api.id();
  const user: HistoryInput = {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'mock-user',
    state: 'accepted',
    content: [{ type: 'text', text: 'Посмотри модуль' }],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT
  };
  const text: HistoryBlock = {
    kind: 'block',
    id: api.id(),
    block_kind: 'text',
    content: 'Смотрю `src/lib`.',
    content_bytes: 'Смотрю `src/lib`.'.length,
    content_truncated: false,
    created_seq: '2',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: run,
    line_count: 1
  };
  const tool: HistoryBlock = {
    kind: 'block',
    id: api.id(),
    block_kind: 'tool_call',
    content: 'ok\n'.repeat(4),
    content_bytes: 12,
    content_truncated: false,
    created_seq: '3',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: 'Read',
    summary: 'src/lib/timeline',
    parent_block_id: null,
    input_command_id: run,
    line_count: 4
  };
  api.histories.set(id, [user, text, tool]);
  api.blocks.set(text.id, { text: text.content, rev: 1, state: 'final' });
  api.blocks.set(tool.id, { text: tool.content, rev: 1, state: 'final' });
  api.bindConversation(id);
  return id;
}

export function seedMockup06(api: FakeHubApi): string {
  const id = seedConversation(api, {
    title: 'Граница snapshot / replay',
    model: 'opus-5.5',
    effort: 'high',
    permission_mode: 'ask',
    activity: {
      state: 'waiting_for_user',
      evidence: { question: 'Что делать, если курсор старше журнала событий?' },
      since: AT
    }
  });
  const detail = api.details.get(id)!;
  detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  detail.machine = { id: 'workstation', name: 'workstation' };
  api.hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    payload: {
      request_id: api.id(),
      kind: 'question',
      payload: {
        questions: [
          {
            id: 'q0',
            text: 'Что делать, если курсор старше журнала событий?',
            options: [{ label: 'Resync', description: 'Reload the snapshot' }, { label: 'Replay', description: 'Read the journal' }],
            multi_select: false,
            allow_custom: true
          }
        ]
      }
    }
  });
  return id;
}

export function seedMockup08(api: FakeHubApi): string {
  const id = seedConversation(api, {
    title: 'Восстановление воркера',
    model: 'opus-5.5',
    effort: 'medium',
    permission_mode: 'default',
    activity: {
      state: 'quota_paused',
      evidence: { reason: 'weekly_reserve' },
      since: AT
    },
    crunch: {
      enabled: true
    }
  });
  const detail = api.details.get(id)!;
  detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  detail.machine = { id: 'mini', name: 'mini' };
  detail.crunch = {
    enabled: true,
    resume_pending: true,
    resume_text: 'Лимиты восстановлены, продолжай с того места, где остановился.'
  };
  detail.queue = {
    pause_reasons: [],
    hold_reasons: [
      { reason: 'weekly_reserve', provider: 'claude', resets_at: '2026-10-30T09:00:00.000Z' },
      { reason: 'quota_exhausted', provider: 'claude', wake_at: '2026-10-29T01:30:00.000Z', window: '5h' }
    ],
    queued: 0,
    run_slot: 'free'
  };
  return id;
}

export function seedMockup04(api: FakeHubApi): { id: string; runId: string } {
  const id = seedConversation(api, {
    title: 'Восстановление потока',
    model: 'opus-5.5'
  });
  const detail = api.details.get(id)!;
  detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  detail.machine = { id: 'workstation', name: 'workstation' };

  const runId = '00000000-0000-4000-8000-000000000040';
  const userInput: HistoryInput = {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'mock-user-4',
    state: 'accepted',
    content: [{ type: 'text', text: 'Проверь поток после обрыва. Дубликатов быть не должно.' }],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT
  };
  const answerBlock: HistoryBlock = {
    kind: 'block',
    id: api.id(),
    block_kind: 'text',
    content: 'Между снимком и подпиской есть окно.',
    content_bytes: 'Между снимком и подпиской есть окно.'.length,
    content_truncated: false,
    created_seq: '2',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'Claude Code',
    model: 'opus-5.5',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: runId,
    line_count: 1
  };
  const toolBlock: HistoryBlock = {
    kind: 'block',
    id: api.id(),
    block_kind: 'tool_call',
    content: 'Операция завершена.',
    content_bytes: 19,
    content_truncated: false,
    created_seq: '3',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'Claude Code',
    model: 'opus-5.5',
    tool_name: 'Read',
    summary: 'src/streams/replay.rs',
    parent_block_id: null,
    input_command_id: runId,
    line_count: 1
  };
  const subagentBlock: HistoryBlock = {
    kind: 'block',
    id: api.id(),
    block_kind: 'tool_call',
    content: 'ok',
    content_bytes: 2,
    content_truncated: false,
    created_seq: '4',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'Explore',
    model: 'haiku',
    tool_name: 'Agent',
    summary: 'Поиск дедупликации',
    parent_block_id: null,
    input_command_id: runId,
    line_count: 1
  };

  api.histories.set(id, [userInput, answerBlock, toolBlock, subagentBlock]);
  api.blocks.set(answerBlock.id, { text: answerBlock.content, rev: 1, state: 'final' });
  api.blocks.set(toolBlock.id, { text: toolBlock.content, rev: 1, state: 'final' });
  api.blocks.set(subagentBlock.id, { text: subagentBlock.content, rev: 1, state: 'final' });

  api.runs.set(runId, {
    run_id: runId,
    input_id: userInput.id,
    started_at: '2026-10-26T22:12:00.000Z',
    finished_at: '2026-10-26T22:14:18.000Z',
    duration_ms: 138000,
    context: { tokens: 120000, window_tokens: 200000 },
    limit: { provider: 'claude', weekly_delta_permille: 4, exact: true },
    parts: [
      { block_id: answerBlock.id, kind: 'main', agent: 'Claude Code', model: 'opus-5.5' },
      { block_id: subagentBlock.id, kind: 'subagent', agent: 'Explore', model: 'haiku', task: 'Поиск дедупликации' }
    ],
    usage: [
      {
        agent: 'Claude Code',
        model: 'opus-5.5',
        input_tokens: 18420,
        output_tokens: 1842,
        cache_read_tokens: 14080,
        cache_write_tokens: 2128
      },
      {
        agent: 'Explore',
        model: 'haiku',
        input_tokens: 4216,
        output_tokens: 386,
        cache_read_tokens: 3104,
        cache_write_tokens: 0
      }
    ]
  });

  api.bindConversation(id);
  return { id, runId };
}

export function seedMockup05(api: FakeHubApi): { id: string; rootBlockId: string } {
  const id = seedConversation(api, {
    title: 'Поиск дедупликации',
    model: 'opus-5.5'
  });
  const detail = api.details.get(id)!;
  detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  detail.machine = { id: 'workstation', name: 'workstation' };

  const rootBlockId = 'tool-agent-explore';
  const rootAgentBlock: HistoryBlock = {
    kind: 'block',
    id: rootBlockId,
    block_kind: 'tool_call',
    content: 'Запущено',
    content_bytes: 8,
    content_truncated: false,
    created_seq: '1',
    created_at: AT,
    rev: 1,
    state: 'streaming',
    agent: 'Explore',
    model: 'haiku',
    tool_name: 'Agent',
    summary: 'Найди повторное применение событий. Код не меняй.',
    parent_block_id: null,
    input_command_id: null,
    line_count: 1
  };

  const childTool: HistoryBlock = {
    kind: 'block',
    id: 'child-read-1',
    block_kind: 'tool_call',
    content: 'Операция завершена.',
    content_bytes: 19,
    content_truncated: false,
    created_seq: '2',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'Explore',
    model: 'haiku',
    tool_name: 'Read',
    summary: 'src/streams/blocks.rs',
    parent_block_id: rootBlockId,
    input_command_id: null,
    line_count: 1
  };

  const childText: HistoryBlock = {
    kind: 'block',
    id: 'child-text-1',
    block_kind: 'text',
    content: 'В replay.rs курсор меняется до применения блока:',
    content_bytes: 47,
    content_truncated: false,
    created_seq: '3',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'Explore',
    model: 'haiku',
    tool_name: null,
    summary: null,
    parent_block_id: rootBlockId,
    input_command_id: null,
    line_count: 1
  };

  api.histories.set(id, [rootAgentBlock, childTool, childText]);
  api.blocks.set(rootBlockId, { text: rootAgentBlock.content, rev: 1, state: 'streaming' });
  api.blocks.set(childTool.id, { text: childTool.content, rev: 1, state: 'final' });
  api.blocks.set(childText.id, { text: childText.content, rev: 1, state: 'final' });

  api.bindConversation(id);
  return { id, rootBlockId };
}

export function seedMockup11(api: FakeHubApi): { id: string } {
  const { id } = seedMockup04(api);
  api.delegations.set(id, [
    {
      block_id: 'tool-agent-explore',
      parent_block_id: null,
      started_at: AT,
      finished_at: null,
      state: 'running',
      tool_name: 'Read',
      task: 'дедупликация',
      subagent: { name: 'Explore', model: 'haiku' }
    },
    {
      block_id: 'tool-agent-codex',
      parent_block_id: null,
      started_at: AT,
      finished_at: null,
      state: 'waiting',
      tool_name: null,
      task: 'тесты',
      subagent: { name: 'Codex', model: 'gpt-6.1-sol' }
    }
  ]);
  return { id };
}
