import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e2';
const AT = '2026-11-05T12:00:00.000Z';
const BIG = 2 * 1024 * 1024 + 1;

const DOC = [
  '# Отчёт по сборке',
  '',
  'Сборка прошла **успешно**.',
  '',
  '| Шаг | Время |',
  '| --- | --- |',
  '| Сборка | 12 с |',
  '| Тесты | 40 с |',
  '',
  '- [x] Проверки',
  '- [ ] Выкладка',
  '',
  '```rust',
  'fn main() {',
  '    println!("готово");',
  '}',
  '```',
  '',
  '[Локальный файл](docs/readme.md)',
  '',
  '[Документация](https://example.com/docs)',
  '',
  '![Схема сборки](images/scheme.png)',
  '',
  '[Опасная ссылка](javascript:alert(1))',
  '',
  '<script>window.__mdScriptRan = true</script>'
].join('\n');

const OWNER_DOC = '# Заметки владельца\n\nФайл прислан из композера.';

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
    line_count: content.split('\n').length
  };
}

function sharedBlock(api: FakeHubApi): HistoryBlock {
  const block = textBlock(api, '');
  block.block_kind = 'tool_call';
  block.tool_name = 'share_file';
  block.summary = 'Документы';
  block.files = [
    {
      attachment_id: 'agent-doc',
      name: 'report.md',
      mime: 'text/markdown',
      size: Buffer.byteLength(DOC),
      title: 'Отчёт',
      note: '',
      width: null,
      height: null
    },
    {
      attachment_id: 'agent-big',
      name: 'huge.md',
      mime: 'text/markdown',
      size: BIG,
      title: 'Большой отчёт',
      note: '',
      width: null,
      height: null
    }
  ];
  return block;
}

function ownerInput(api: FakeHubApi): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'markdown-owner',
    state: 'accepted',
    content: [{ type: 'text', text: 'файл от владельца' }],
    attachments: [
      {
        attachment_id: 'owner-doc',
        name: 'Заметки.MD',
        mime: 'text/plain',
        size: Buffer.byteLength(OWNER_DOC),
        sha256: '',
        image: false
      }
    ],
    created_seq: '2',
    feed_seq: '2',
    created_at: AT
  };
}

async function boot(page: import('@playwright/test').Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  api.histories.set(id, [sharedBlock(api), ownerInput(api)]);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

async function interceptAttachments(page: import('@playwright/test').Page, requested: string[]) {
  await page.route('**/api/v1/attachments/*/content', async (route) => {
    const path = new URL(route.request().url()).pathname;
    requested.push(path);
    const body = path.includes('/owner-doc/') ? OWNER_DOC : DOC;
    await route.fulfill({ status: 200, contentType: 'text/markdown; charset=utf-8', body });
  });
}

async function stubClipboard(page: import('@playwright/test').Page) {
  await page.addInitScript(() => {
    const write = async (text: string) => {
      (window as unknown as { __copied?: string }).__copied = text;
    };
    const clipboard = navigator.clipboard;
    if (clipboard) Object.defineProperty(clipboard, 'writeText', { value: write, configurable: true });
    else Object.defineProperty(navigator, 'clipboard', { value: { writeText: write }, configurable: true });
  });
}

test('an_agent_shared_markdown_opens_rendered_with_a_source_toggle_copy_and_download', async ({ page }) => {
  const requested: string[] = [];
  await stubClipboard(page);
  await boot(page);
  await interceptAttachments(page, requested);

  const tile = page.getByRole('button', { name: 'Открыть документ report.md' });
  await expect(tile).toBeVisible();
  // В ленте у плитки свой доступ к скачиванию, не только внутри просмотрщика.
  const feedDownload = page.getByRole('link', { name: 'Скачать report.md' });
  await expect(feedDownload).toHaveAttribute('href', '/api/v1/attachments/agent-doc/content');
  await expect(feedDownload).toHaveAttribute('download', 'report.md');

  await tile.click();
  const dialog = page.getByRole('dialog', { name: 'report.md' });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('h1')).toHaveText('Отчёт по сборке');
  await expect(dialog.locator('table')).toContainText('Сборка');
  await expect(dialog.locator('table')).toContainText('40 с');
  await expect(dialog).toContainText('☒ Проверки');
  await expect(dialog.locator('pre code.hljs')).toContainText('println!');

  // Локальный путь некуда вести; безопасная внешняя ссылка открывается в новой вкладке.
  const external = dialog.getByRole('link', { name: 'Локальный файл' });
  await expect(external).toHaveCount(0);
  await expect(dialog.getByText('Локальный файл')).toBeVisible();
  const docs = dialog.getByRole('link', { name: 'Документация' });
  await expect(docs).toHaveAttribute('target', '_blank');
  await expect(docs).toHaveAttribute('rel', /noopener/);
  await expect(dialog.getByText('Схема сборки')).toBeVisible();
  await expect(dialog.locator('img')).toHaveCount(0);
  await expect(dialog.getByText('Опасная ссылка')).toBeVisible();
  await expect(dialog.locator('a[href^="javascript"]')).toHaveCount(0);
  await expect(dialog.locator('script')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __mdScriptRan?: boolean }).__mdScriptRan)).toBeUndefined();

  const download = dialog.getByRole('link', { name: 'Скачать report.md' });
  await expect(download).toHaveAttribute('href', '/api/v1/attachments/agent-doc/content');
  await expect(download).toHaveAttribute('download', 'report.md');

  await dialog.getByRole('button', { name: 'Копировать текст' }).click();
  await expect(dialog.getByRole('button', { name: 'Скопировано' })).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __copied?: string }).__copied)).toBe(DOC);

  await dialog.getByText('Исходник', { exact: true }).click();
  await expect(dialog.locator('pre.source')).toContainText('# Отчёт по сборке');
  await expect(dialog.locator('h1')).toHaveCount(0);

  await dialog.getByText('Просмотр', { exact: true }).click();
  await expect(dialog.locator('h1')).toHaveText('Отчёт по сборке');

  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(tile).toBeFocused();
});

test('a_markdown_file_from_the_owner_opens_in_the_same_viewer', async ({ page }) => {
  const requested: string[] = [];
  await boot(page);
  await interceptAttachments(page, requested);

  const tile = page.getByRole('button', { name: 'Открыть документ Заметки.MD' });
  await expect(tile).toBeVisible();
  await tile.click();

  const dialog = page.getByRole('dialog', { name: 'Заметки.MD' });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('h1')).toHaveText('Заметки владельца');
  await expect(dialog).toContainText('Файл прислан из композера');
  expect(requested).toContain('/api/v1/attachments/owner-doc/content');
});

test('a_document_over_two_mebibytes_offers_download_without_reading_it', async ({ page }) => {
  const requested: string[] = [];
  await boot(page);
  await interceptAttachments(page, requested);

  await page.getByRole('button', { name: 'Открыть документ huge.md' }).click();
  const dialog = page.getByRole('dialog', { name: 'huge.md' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('Файл большой для просмотра');
  await expect(dialog.getByRole('link', { name: 'Скачать huge.md' })).toHaveAttribute(
    'href',
    '/api/v1/attachments/agent-big/content'
  );
  await expect(dialog.getByRole('radio', { name: 'Исходник' })).toHaveCount(0);
  expect(requested.some((path) => path.includes('/agent-big/'))).toBe(false);
});
