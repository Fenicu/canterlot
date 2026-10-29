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
