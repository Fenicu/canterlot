import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e1';
const AT = '2026-11-02T12:00:00.000Z';

const PNG_2X2 = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGPQmNbz//9/BggFADYPCI8NpaO2AAAAAElFTkSuQmCC',
  'base64'
);

function sharedBlock(api: FakeHubApi): HistoryBlock {
  const block = textBlock(api, '');
  block.block_kind = 'tool_call';
  block.tool_name = 'share_file';
  block.summary = 'Отчёт';
  block.files = [
    {
      attachment_id: 'shared-image',
      name: 'screenshot.png',
      mime: 'image/png',
      size: 2048,
      title: 'Снимок экрана',
      note: '',
      width: 2,
      height: 2
    },
    {
      attachment_id: 'shared-report',
      name: 'report.pdf',
      mime: 'application/pdf',
      size: 1536,
      title: 'Отчёт',
      note: 'Смотри раздел 2',
      width: null,
      height: null
    }
  ];
  return block;
}

function textBlock(api: FakeHubApi, content: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content,
    content_bytes: content.length,
    content_truncated: false,
    created_seq: '1',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: 0
  };
}

test('shared_file_block_opens_an_image_and_downloads_other_files', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const block = sharedBlock(api);
  api.histories.set(id, [block]);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.route('**/api/v1/attachments/shared-image/content', async (route) => {
    await route.fulfill({ contentType: 'image/png', body: PNG_2X2 });
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  const row = page.locator(`[data-key="block:${block.id}"]`);
  const files = row.locator('.files li');
  await expect(files).toHaveCount(2);

  await expect(files.nth(0).locator('img')).toBeVisible();
  await files.nth(0).locator('button.thumb').click();
  const dialog = page.getByRole('dialog', { name: 'Файл' });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.slide[data-current] img.full')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);

  const report = files.nth(1).locator('a[download]');
  await expect(report).toContainText('report.pdf');
  await expect(report).toContainText(/1,5\s*КиБ/);
  await expect(report).toContainText('PDF');
  await expect(report).toHaveAttribute(
    'href',
    '/api/v1/attachments/shared-report/content'
  );
  await expect(report).toHaveAttribute('download', 'report.pdf');

  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth
  );
  expect(overflow).toBeLessThanOrEqual(0);
});
