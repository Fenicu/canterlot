import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import type { FakeHubApi } from '../fake-hub/api';

const AT = '2026-11-10T12:00:00.000Z';
const VERIFY_UUID = '01a1dfb1-10bc-7230-8b59-a74abd632567';

export const VERIFY_URL = `https://portal.anthropic.com/verify?org=${VERIFY_UUID}`;
export const VERIFY_TEXT = `403: API Error: Your account can't receive replies from Claude right now. You can find the next step in the Anthropic Verification Portal: ${VERIFY_URL}`;
export const OVERLOADED_TEXT = '529: API Error: Overloaded';
export const ADAPTER_TEXT =
  'Запуск остановлен: делегированной сессии доступны чужие серверы MCP (mcp__foreign__x), а разрешены только canterlot.';

export type ApiErrorSeed = {
  verifyBlock: string;
  overloadedBlock: string;
  adapterBlock: string;
};

type BlockInit = {
  seq: number;
  kind: 'text' | 'error';
  text: string;
  run: string | null;
};

function block(api: FakeHubApi, init: BlockInit): HistoryBlock {
  const id = api.id();
  const item: HistoryBlock = {
    kind: 'block',
    id,
    block_kind: init.kind,
    content: init.text,
    content_bytes: init.text.length,
    content_truncated: false,
    created_seq: String(init.seq),
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'orchestrator',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: init.run,
    line_count: init.text.split('\n').length
  };
  api.blocks.set(id, { text: init.text, rev: 1, state: 'final' });
  return item;
}

/**
 * Ошибки API провайдера в ленте: проверка аккаунта Claude, перегрузка 529
 * и обычная ошибка адаптера, которая остаётся уведомлением в карточке ответа.
 */
export function seedApiErrors(api: FakeHubApi, conversationId: string): ApiErrorSeed {
  const verify = block(api, { seq: 1, kind: 'error', text: VERIFY_TEXT, run: api.id() });
  const overloaded = block(api, { seq: 2, kind: 'error', text: OVERLOADED_TEXT, run: api.id() });
  const run = api.id();
  const answer = block(api, { seq: 3, kind: 'text', text: 'Проверил окружение и остановился.', run });
  const adapter = block(api, { seq: 4, kind: 'error', text: ADAPTER_TEXT, run });
  api.histories.set(conversationId, [verify, overloaded, answer, adapter]);
  return { verifyBlock: verify.id, overloadedBlock: overloaded.id, adapterBlock: adapter.id };
}
