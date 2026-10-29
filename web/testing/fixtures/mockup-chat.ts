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
