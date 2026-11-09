import type { Page } from '@playwright/test';
import { sha256Hex } from '../../src/lib/stores/blocks';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

// Один вызов share_file — одна карточка файла: блок-носитель воркера (`share_file`,
// без модели, у старых записей `main` и без хода) прячется за вызовом модели
// `mcp__canterlot__share_file`, а не открывает отдельное сообщение «Claude».

const OWNER = '00000000-0000-4000-8000-0000000000e2';
const AT = '2026-11-09T12:13:22.000Z';
const TITLE = 'Файл (уже не пустой)';
const ATTACHMENT = '01a1c03a-3481-711a-a729-0000000000aa';
const INPUT = JSON.stringify({ path: '/tmp/empty.txt', title: TITLE });
const ANSWER = JSON.stringify({ status: 'shared', attachment_id: ATTACHMENT, name: 'empty.txt', mime: 'text/plain', size: 195, kind: 'file', title: TITLE, note: '' });
const FILE = { attachment_id: ATTACHMENT, name: 'empty.txt', mime: 'text/plain', size: 195, title: TITLE, note: '', width: null, height: null };

function historyBlock(api: FakeHubApi, patch: Partial<HistoryBlock> & { content: string }): HistoryBlock {
  const id = patch.id ?? api.id();
  api.blocks.set(id, { text: patch.content, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content_bytes: patch.content.length,
    content_truncated: false,
    created_seq: '1',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'orchestrator',
    model: 'claude-opus-5-5',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: 0,
    ...patch
  };
}

async function open(page: Page, hub: FakeHub, api: FakeHubApi, id: string): Promise<void> {
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
}

async function expectOneShareCard(page: Page): Promise<void> {
  const feed = page.locator('.feed');
  await expect(feed.locator('.files li')).toHaveCount(1);
  await expect(feed.locator('[data-row][data-kind="answer"]')).toHaveCount(1);
  await expect(feed.getByText(`Передала файл «${TITLE}»`)).toHaveCount(1);
  const link = feed.locator('.files a[download]');
  await expect(link).toHaveAttribute('href', `/api/v1/attachments/${ATTACHMENT}/content`);
  await expect(link).toHaveAttribute('download', 'empty.txt');
  await expect(link).toContainText('empty.txt');
}

test('history_with_both_share_blocks_shows_one_card_in_the_turn', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const run = api.id();
  const call = historyBlock(api, { block_kind: 'tool_call', tool_name: 'mcp__canterlot__share_file', content: INPUT, input_command_id: run, created_seq: '2' });
  const carrier = historyBlock(api, { block_kind: 'tool_call', tool_name: 'share_file', summary: TITLE, content: '', agent: 'main', model: '', created_seq: '3', files: [FILE] });
  api.histories.set(id, [
    historyBlock(api, { content: 'Передаю файл.', input_command_id: run }),
    call,
    carrier,
    historyBlock(api, { block_kind: 'tool_result', content: ANSWER, parent_block_id: call.id, input_command_id: run, created_seq: '4' }),
    historyBlock(api, { content: 'Готово.', input_command_id: run, created_seq: '5' })
  ]);
  await open(page, hub, api, id);
  await expect(page.locator('.feed')).toContainText('Готово.');
  await expect(page.locator(`[data-key="block:${carrier.id}"]`)).toHaveCount(0);
  await expectOneShareCard(page);

  const download = page.waitForEvent('download');
  await page.route(`**/api/v1/attachments/${ATTACHMENT}/content`, (route) =>
    route.fulfill({ contentType: 'text/plain', headers: { 'content-disposition': 'attachment; filename="empty.txt"' }, body: 'x'.repeat(195) })
  );
  await page.locator('.feed .files a[download]').click();
  expect((await download).suggestedFilename()).toBe('empty.txt');
});

test('live_share_shows_one_card_while_the_carrier_and_the_result_arrive', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  await open(page, hub, api, id);
  const scope = `conversation:${id}`;
  const run = api.id();
  const started = (block_id: string, kind: string, patch: Record<string, unknown> = {}) => ({
    kind: 'block_started' as const,
    conversation_id: id,
    payload: { block_id, rev: 0, kind, input_command_id: run, parent_block_id: null, agent: 'orchestrator', model: 'claude-opus-5-5', tool_name: null, summary: null, approved_by: null, ...patch }
  });
  const final = async (block_id: string, text: string) => ({
    kind: 'block_final' as const,
    conversation_id: id,
    payload: { block_id, rev: 1, text, checksum: await sha256Hex(text), text_omitted: false, summary: null }
  });

  const lead = api.id();
  hub.append(scope, started(lead, 'text'));
  hub.append(scope, await final(lead, 'Передаю файл.'));
  const call = api.id();
  hub.append(scope, started(call, 'tool_call', { tool_name: 'mcp__canterlot__share_file' }));
  hub.append(scope, await final(call, INPUT));
  hub.flush();
  await expect(page.locator('.feed')).toContainText('Передаю файл.');

  // Носитель нового воркера: в том же ходе, метка основного агента.
  const carrier = api.id();
  hub.append(scope, started(carrier, 'tool_call', { tool_name: 'share_file', summary: TITLE, model: '' }));
  hub.append(scope, await final(carrier, ''));
  hub.append(scope, { kind: 'block_file_attached', conversation_id: id, payload: { block_id: carrier, ...FILE } });
  hub.flush();
  await expect(page.locator(`[data-key="block:${carrier}"]`)).toHaveCount(0);
  await expectOneShareCard(page);

  const result = api.id();
  hub.append(scope, started(result, 'tool_result', { parent_block_id: call }));
  hub.append(scope, await final(result, ANSWER));
  const tail = api.id();
  hub.append(scope, started(tail, 'text'));
  hub.append(scope, await final(tail, 'Готово.'));
  hub.flush();
  await expect(page.locator('.feed')).toContainText('Готово.');
  await expectOneShareCard(page);
});
