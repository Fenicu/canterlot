import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

// Отчёт владельца 2026-11-09 (Codex, 1.24.1): карточка из одного носителя файла
// `share_file` (без модели, без хода, `main`) показывала (i), а «Об ответе» — «Не найдено».
// Codex зовёт MCP `canterlot.share_file`; носитель прежнего воркера хода не знает.

const OWNER = '00000000-0000-4000-8000-0000000000e3';
const AT = Date.parse('2026-11-09T14:13:00.000Z');
const TITLE = 'Улучшенный скриншот — JPG';
const ATTACHMENT = '01a1c03a-3481-711a-a729-0000000000bb';
const FILE = { attachment_id: ATTACHMENT, name: 'shot.jpg', mime: 'image/jpeg', size: 1_363_148, title: TITLE, note: '', width: 1920, height: 1080 };
const INFO = 'Сведения о ходе';

let seq = 0;

function block(api: FakeHubApi, patch: Partial<HistoryBlock> & { content: string; at?: number }): HistoryBlock {
  const id = patch.id ?? api.id();
  api.blocks.set(id, { text: patch.content, rev: 1, state: 'final' });
  seq += 1;
  const { at, ...rest } = patch;
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content_bytes: patch.content.length,
    content_truncated: false,
    created_seq: String(seq),
    created_at: new Date(at ?? AT).toISOString(),
    rev: 1,
    state: 'final',
    agent: 'orchestrator',
    model: 'gpt-5.5-codex',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: 0,
    ...rest
  };
}

function ownerInput(api: FakeHubApi, text: string, at: number): HistoryInput {
  seq += 1;
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `owner-${seq}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: String(seq),
    feed_seq: String(seq),
    created_at: new Date(at).toISOString()
  } as HistoryInput;
}

/** Носитель воркера 1.24.1: `share_file`, без модели и хода, метка `main`. */
function oldCarrier(api: FakeHubApi, at: number): HistoryBlock {
  return block(api, { block_kind: 'tool_call', tool_name: 'share_file', summary: TITLE, content: '', agent: 'main', model: '', files: [FILE], at });
}

function seedRun(api: FakeHubApi, run: string, input: string): void {
  api.runs.set(run, {
    run_id: run,
    input_id: input,
    started_at: new Date(AT).toISOString(),
    finished_at: new Date(AT + 60_000).toISOString(),
    duration_ms: 60_000,
    context: null,
    limit: null,
    parts: [],
    usage: [{ agent: 'orchestrator', model: 'gpt-5.5-codex', input_tokens: 1200, output_tokens: 300, cache_read_tokens: 0, cache_write_tokens: 0 }]
  } as never);
}

async function open(page: Page, hub: FakeHub, api: FakeHubApi, id: string): Promise<void> {
  const detail = api.details.get(id);
  if (detail) detail.harness = { id: 'codex', title: 'Codex', provider: 'openai' };
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
}

async function expectSheetWithoutError(page: Page): Promise<void> {
  const sheet = page.getByRole('dialog', { name: 'Об ответе' });
  await expect(sheet).toBeVisible();
  await expect(sheet.getByText('Загрузка сведений…')).toHaveCount(0);
  await expect(sheet).not.toContainText('Не найдено');
  await expect(sheet.locator('.error-msg')).toHaveCount(0);
}

test('an_unpaired_codex_carrier_joins_the_answer_and_its_info_describes_that_turn', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const run = api.id();
  const input = ownerInput(api, 'Улучши скриншот', AT - 1_000);
  seedRun(api, run, input.id);
  const call = block(api, { block_kind: 'tool_call', tool_name: 'canterlot.share_file', content: JSON.stringify({ path: '/tmp/shot.jpg', title: TITLE }), input_command_id: run });
  // Носитель записан позже получаса после вызова: пара не складывается.
  const carrier = oldCarrier(api, AT + 45 * 60_000);
  api.histories.set(id, [
    input,
    block(api, { content: 'Готовлю скриншот.', input_command_id: run }),
    call,
    block(api, { content: 'Готово.', input_command_id: run, at: AT + 44 * 60_000 }),
    carrier
  ]);
  await open(page, hub, api, id);
  const feed = page.locator('.feed');
  await expect(feed).toContainText('Готово.');
  await expect(feed.locator(`[data-key="block:${carrier.id}"]`)).toHaveCount(1);
  await expect(feed.locator('[data-row][data-kind="answer"]')).toHaveCount(1);
  const buttons = feed.getByRole('button', { name: INFO });
  await expect(buttons).toHaveCount(1);
  await buttons.click();
  await expectSheetWithoutError(page);
  await expect(page.getByRole('dialog', { name: 'Об ответе' })).toContainText('Ход завершён');
});

test('a_lone_carrier_without_a_turn_has_no_info_button', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const run = api.id();
  const input = ownerInput(api, 'Улучши скриншот', AT - 1_000);
  seedRun(api, run, input.id);
  const carrier = oldCarrier(api, AT + 2_000);
  api.histories.set(id, [
    input,
    block(api, { content: 'Готовлю скриншот.', input_command_id: run }),
    // Сообщение владельца между ответом и носителем: соседней карточки у носителя нет.
    ownerInput(api, 'Жду', AT + 1_000),
    carrier
  ]);
  await open(page, hub, api, id);
  const feed = page.locator('.feed');
  const lone = feed.locator('[data-row][data-kind="answer"]').filter({ has: page.locator(`[data-key="block:${carrier.id}"]`) });
  await expect(lone).toHaveCount(1);
  await expect(lone.locator('[data-role="answer-footer"]')).toContainText('Codex');
  await expect(lone.getByRole('button', { name: INFO })).toHaveCount(0);

  // Каждая (i), что осталась в ленте, открывает сведения без ошибки.
  const buttons = feed.getByRole('button', { name: INFO });
  await expect(buttons).toHaveCount(1);
  await buttons.click();
  await expectSheetWithoutError(page);
});

test('a_turn_the_hub_does_not_know_says_the_details_are_unavailable', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const run = api.id();
  api.histories.set(id, [ownerInput(api, 'Привет', AT - 1_000), block(api, { content: 'Ответ без сведений.', input_command_id: run })]);
  await open(page, hub, api, id);
  await page.locator('.feed').getByRole('button', { name: INFO }).click();
  const sheet = page.getByRole('dialog', { name: 'Об ответе' });
  await expect(sheet.locator('[data-role="info-missing"]')).toHaveText('Сведения об этом ответе недоступны');
  await expect(sheet).not.toContainText('Не найдено');
  await expect(sheet.locator('.error-msg')).toHaveCount(0);
});
