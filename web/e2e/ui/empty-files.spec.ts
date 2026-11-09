import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

// Пустой файл (.gitkeep, пустой журнал) — законное вложение в обе стороны.
const OWNER = '00000000-0000-4000-8000-0000000000e7';
const AT = '2026-11-09T12:00:00.000Z';
const EMPTY_SHA = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';

function shareBlock(api: FakeHubApi): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: '', rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'tool_call',
    content: '',
    content_bytes: 0,
    content_truncated: false,
    created_seq: '1',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: 'share_file',
    summary: 'empty.txt',
    parent_block_id: null,
    input_command_id: null,
    line_count: 0,
    files: [
      { attachment_id: 'shared-empty', name: 'empty.txt', mime: 'text/plain', size: 0, title: '', note: '', width: null, height: null },
      { attachment_id: 'shared-empty-png', name: 'empty.png', mime: 'application/octet-stream', size: 0, title: '', note: '', width: null, height: null }
    ]
  };
}

test('an_empty_file_is_attached_without_a_body_and_sent', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  const created: Record<string, unknown>[] = [];
  const patches: string[] = [];
  const inputs: Record<string, unknown>[] = [];
  await page.route('**/api/v1/attachments**', async (route) => {
    const request = route.request();
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/v1/attachments') {
      created.push(request.postDataJSON() as Record<string, unknown>);
    }
    if (request.method() === 'PATCH') patches.push(request.url());
    await route.fallback();
  });
  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() === 'POST') inputs.push(route.request().postDataJSON() as Record<string, unknown>);
    await route.fallback();
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  await page.setInputFiles('input[type=file]', { name: 'empty.txt', mimeType: 'text/plain', buffer: Buffer.alloc(0) });
  const tray = page.locator('[data-composer] .tray');
  await expect(tray.getByText('empty.txt')).toBeVisible();
  // Хеш считается в воркере: под нагрузкой до готовности проходит не одна секунда.
  await expect(tray.locator('li[data-phase="ready"]')).toHaveCount(1, { timeout: 30_000 });
  await expect(tray.locator('.meta')).toHaveText('0 Б · Загружено');
  const bar = page.getByRole('progressbar', { name: 'empty.txt' });
  await expect(bar).toHaveAttribute('aria-valuenow', '1');
  await expect(bar).toHaveAttribute('aria-valuemax', '1');
  await expect(page.getByText('Файл пустой')).toHaveCount(0);
  expect(created).toHaveLength(1);
  expect(created[0]).toMatchObject({ name: 'empty.txt', size: 0, sha256: EMPTY_SHA });
  expect(patches).toHaveLength(0);

  await page.getByRole('textbox', { name: 'Сообщение' }).fill('пустой файл');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => inputs.length).toBe(1);
  expect((inputs[0]?.attachment_ids as string[] | undefined) ?? []).toHaveLength(1);
});

test('an_empty_shared_file_is_a_download_card_with_zero_bytes', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const block = shareBlock(api);
  api.histories.set(id, [block]);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  const files = page.locator(`[data-key="block:${block.id}"] .files li`);
  await expect(files).toHaveCount(2);
  // Пустой файл не открывается как картинка даже с именем .png: только скачивание.
  await expect(files.locator('img')).toHaveCount(0);
  for (const [index, name, attachment] of [[0, 'empty.txt', 'shared-empty'], [1, 'empty.png', 'shared-empty-png']] as const) {
    const link = files.nth(index).locator('a[download]');
    await expect(link).toContainText(name);
    await expect(link.locator('.meta')).toHaveText('0 Б');
    await expect(link).toHaveAttribute('href', `/api/v1/attachments/${attachment}/content`);
    await expect(link).toHaveAttribute('download', name);
  }
});
