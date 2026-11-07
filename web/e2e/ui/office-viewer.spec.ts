import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e3';
const AT = '2026-11-07T12:00:00.000Z';
const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';
const XLSX = 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
const fixture = (name: string) => readFileSync(resolve(import.meta.dirname, '../../testing/fixtures/office', name));

/** Вложение → файл фикстуры и тип, с которым его отдаёт хаб. */
const FILES: Record<string, { file: string; mime: string; name: string }> = {
  'share-docx': { file: 'report.docx', mime: DOCX, name: 'report.docx' },
  'share-broken': { file: 'broken.docx', mime: DOCX, name: 'broken.docx' },
  'share-legacy': { file: 'legacy.doc', mime: 'application/msword', name: 'legacy.doc' },
  'share-locked': { file: 'locked.docx', mime: DOCX, name: 'locked.docx' },
  'answer-xlsx': { file: 'table.xlsx', mime: 'application/octet-stream', name: 'table.xlsx' },
  'owner-docx': { file: 'report.docx', mime: DOCX, name: 'Заметки.docx' }
};
const BIG = { attachment_id: 'share-big', name: 'huge.xlsx', mime: XLSX, size: 10 * 1024 * 1024 + 1 };

function block(api: FakeHubApi, patch: Partial<HistoryBlock>): HistoryBlock {
  const id = api.id();
  const content = patch.content ?? '';
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
    line_count: 0,
    ...patch
  };
}

const blockFile = (id: string, size = fixture(FILES[id]!.file).length, over: Partial<{ name: string; mime: string }> = {}) => ({
  attachment_id: id,
  name: over.name ?? FILES[id]!.name,
  mime: over.mime ?? FILES[id]!.mime,
  size,
  title: '',
  note: '',
  width: null,
  height: null
});

/** share_file с карточкой, ответ агента обычным блоком инструмента и вложение владельца. */
function history(api: FakeHubApi): { items: (HistoryBlock | HistoryInput)[]; share: string; answer: string } {
  const share = block(api, {
    block_kind: 'tool_call',
    tool_name: 'mcp__canterlot__share_file',
    summary: 'mcp__canterlot__share_file',
    content: JSON.stringify({ path: '/tmp/office/report.docx', title: 'Документы проверки' })
  });
  share.files = [
    blockFile('share-docx'),
    blockFile('share-broken'),
    blockFile('share-legacy'),
    blockFile('share-locked'),
    { ...BIG, title: '', note: '', width: null, height: null }
  ];
  const shareResult = block(api, { block_kind: 'tool_result', parent_block_id: share.id, content: JSON.stringify({ status: 'shared', title: 'Документы проверки' }) });
  const answer = block(api, { block_kind: 'tool_call', tool_name: 'export_report', summary: 'export_report: таблица', content: JSON.stringify({ format: 'xlsx' }) });
  answer.files = [blockFile('answer-xlsx')];
  const owner: HistoryInput = {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'office-owner',
    state: 'accepted',
    content: [{ type: 'text', text: 'Вот заметки' }],
    attachments: [{ attachment_id: 'owner-docx', name: 'Заметки.docx', mime: DOCX, size: fixture('report.docx').length, sha256: '', image: false }],
    created_seq: '3',
    feed_seq: '3',
    created_at: AT
  };
  return { items: [owner, share, shareResult, answer], share: share.id, answer: answer.id };
}

async function boot(page: Page) {
  const requested: string[] = [];
  // Каждый выданный и отозванный адрес blob: запоминается: картинки документа должны освобождаться при закрытии.
  await page.addInitScript(() => {
    const made: string[] = [];
    const revoked: string[] = [];
    const create = URL.createObjectURL.bind(URL);
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (object: Blob | MediaSource) => {
      const url = create(object);
      made.push(url);
      return url;
    };
    URL.revokeObjectURL = (url: string) => {
      revoked.push(url);
      revoke(url);
    };
    Object.assign(window, { __blobs: { made, revoked } });
  });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const seed = history(api);
  api.histories.set(id, seed.items);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.route('**/api/v1/attachments/*/content', async (route) => {
    const attachment = decodeURIComponent(new URL(route.request().url()).pathname.split('/')[4] ?? '');
    requested.push(attachment);
    const entry = FILES[attachment];
    if (!entry) return route.fulfill({ status: 404 });
    await route.fulfill({ status: 200, contentType: entry.mime, body: fixture(entry.file) });
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { requested, ...seed };
}

const blobs = (page: Page) => page.evaluate(() => (window as unknown as { __blobs: { made: string[]; revoked: string[] } }).__blobs);

async function noPageOverflow(page: Page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
}

async function openTile(page: Page, scope: Locator, label: string) {
  const tile = scope.getByRole('button', { name: label });
  await tile.scrollIntoViewIfNeeded();
  await tile.click();
  return tile;
}

test('a_docx_from_a_share_file_card_renders_headings_lists_a_table_a_blob_image_and_safe_links', async ({ page }) => {
  const { share } = await boot(page);
  const card = page.locator(`[data-key="block:${share}"]`);
  await expect(card.locator('.feed-card')).toHaveCount(1);
  const tile = card.getByRole('button', { name: 'Открыть документ report.docx' });
  await expect(tile).toContainText('Документ Word');
  await expect(tile.locator('.glyph')).toHaveText('DOCX');
  await openTile(page, card, 'Открыть документ report.docx');

  const dialog = page.getByRole('dialog', { name: 'report.docx' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('Документ Word');
  const text = dialog.locator('.document-text');
  await expect(text.locator('h1')).toHaveText('Отчёт о проверке сборки');
  await expect(text.locator('h2').first()).toHaveText('Шаги');
  await expect(text.locator('strong').first()).toHaveText('успешно');
  await expect(text.locator('ul > li')).toHaveCount(3);
  await expect(text.locator('ol > li')).toHaveText(['Поставить метку версии', 'Дождаться зелёной сборки']);
  await expect(text.locator('table')).toContainText('3 мин 40 с');

  const image = dialog.locator('.document-text img');
  await expect(image).toHaveCount(1);
  await expect(image).toHaveAttribute('src', /^blob:/);
  await expect(image).toHaveAttribute('alt', 'Схема сборки');
  await expect.poll(() => image.evaluate((node: HTMLImageElement) => node.complete && node.naturalWidth)).toBe(160);
  const src = await image.getAttribute('src');

  const docs = dialog.getByRole('link', { name: 'Документация' });
  await expect(docs).toHaveAttribute('href', 'https://example.com/docs');
  await expect(docs).toHaveAttribute('target', '_blank');
  await expect(docs).toHaveAttribute('rel', /noopener noreferrer/);
  await expect(dialog.getByText('Опасная ссылка')).toBeVisible();
  await expect(dialog.getByRole('link', { name: 'Опасная ссылка' })).toHaveCount(0);
  await expect(text.getByText('Опасная ссылка')).toHaveCSS('text-decoration-line', 'none');
  await expect(dialog.locator('a[href^="javascript"], script, [style]')).toHaveCount(0);

  // Таблица прокручивается в своей рамке, страница вбок не едет.
  await noPageOverflow(page);
  await expect(dialog.getByRole('link', { name: 'Скачать report.docx' })).toHaveAttribute('href', '/api/v1/attachments/share-docx/content');

  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(tile).toBeFocused();
  await expect.poll(async () => (await blobs(page)).revoked).toContain(src);
});

test('an_xlsx_from_an_agent_answer_switches_sheets_keeps_the_header_sticky_and_notes_the_row_cap', async ({ page }) => {
  const { answer } = await boot(page);
  const row = page.locator(`[data-key="block:${answer}"]`);
  const tile = row.getByRole('button', { name: 'Открыть таблицу table.xlsx' });
  await expect(tile).toContainText('Таблица Excel');
  await openTile(page, row, 'Открыть таблицу table.xlsx');

  const dialog = page.getByRole('dialog', { name: 'table.xlsx' });
  const tabs = dialog.getByRole('tab');
  await expect(tabs).toHaveText(['Сводка', 'Журнал', 'Форматы']);
  await expect(tabs.first()).toHaveAttribute('aria-selected', 'true');
  const grid = dialog.getByRole('tabpanel');
  // Формула B2-B3 показывает сохранённый результат, даты и логические значения — по-русски.
  await expect(grid.locator('thead th')).toHaveText(['Показатель', 'Значение', 'Дата', 'Готово']);
  const profit = grid.locator('tbody tr').nth(2);
  await expect(profit.locator('th, td')).toHaveText(['Прибыль', '420 000,5', '31.10.2026, 18:30', 'ИСТИНА']);
  await expect(grid.locator('tbody tr').first().locator('td')).toHaveText(['1 250 000,5', '01.10.2026', 'ИСТИНА']);
  await expect(grid.locator('tbody tr').first().locator('td').first()).toHaveCSS('text-align', 'right');
  await expect(grid.locator('thead th').nth(1)).toHaveCSS('text-align', 'right');
  await expect(dialog.locator('[data-role="sheet-cap"]')).toHaveCount(0);

  await tabs.nth(1).click();
  await expect(tabs.nth(1)).toHaveAttribute('aria-selected', 'true');
  await expect(dialog.locator('[data-role="sheet-cap"]')).toHaveText('Показаны первые 2000 из 2201 строки. Остальное — в скачанном файле.');
  // Все две тысячи строк в таблице, но в DOM только видимое окно: лист не ждёт раскладки тысяч ячеек.
  await expect(grid.locator('table')).toHaveAttribute('aria-rowcount', '2000');
  expect(await grid.locator('tbody tr[aria-rowindex]').count()).toBeLessThan(200);
  await expect(grid.locator('thead th').first()).toHaveText('Дата');

  // Шапка остаётся наверху, первый столбец — слева, сколько бы ни листали.
  const box = (await grid.boundingBox())!;
  await grid.evaluate((node) => node.scrollTo({ top: node.scrollHeight, left: node.scrollWidth }));
  await expect.poll(() => grid.evaluate((node) => node.scrollTop)).toBeGreaterThan(1000);
  const header = (await grid.locator('thead th').nth(3).boundingBox())!;
  expect(Math.abs(header.y - box.y)).toBeLessThanOrEqual(2);
  const lastRow = grid.locator('tbody tr[aria-rowindex="2000"]');
  await expect(lastRow.locator('td').first()).toHaveText('1 999');
  const firstColumn = (await lastRow.locator('th').boundingBox())!;
  expect(Math.abs(firstColumn.x - box.x)).toBeLessThanOrEqual(2);
  await noPageOverflow(page);

  // Стрелки по вкладкам — как у любого списка вкладок.
  await tabs.nth(1).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(tabs.first()).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.first()).toBeFocused();
  await expect(grid.locator('thead th').first()).toHaveText('Показатель');

  // Числовые форматы из styles.xml: проценты и валюты в ru-RU, а не сырые доли и суммы.
  await tabs.nth(2).click();
  const formatted = grid.locator('tbody tr');
  await expect(formatted.nth(1).locator('td')).toHaveText(['33,6 %']);
  await expect(formatted.nth(3).locator('td')).toHaveText(['1 750,00 ₽']);
  await expect(formatted.nth(5).locator('td')).toHaveText(['1 750,00 $']);
  await expect(formatted.nth(11).locator('td')).toHaveText(['1 234,5']);

  // Системный «назад» закрывает просмотрщик, а не уводит из чата.
  await page.goBack();
  await expect(dialog).toHaveCount(0);
  await expect(row).toBeVisible();
});

test('a_horizontal_touch_swipe_scrolls_the_wide_sheet_and_keeps_the_header_aligned', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'phone', 'настоящее касание проверяется на телефоне');
  const { answer } = await boot(page);
  const row = page.locator(`[data-key="block:${answer}"]`);
  await openTile(page, row, 'Открыть таблицу table.xlsx');
  const dialog = page.getByRole('dialog', { name: 'table.xlsx' });
  await dialog.getByRole('tab', { name: 'Журнал' }).click();
  const grid = dialog.getByRole('tabpanel');
  await expect(dialog.locator('[data-role="sheet-cap"]')).toBeVisible();
  await expect(grid.locator('thead th').first()).toHaveText('Дата');

  // Лист не должен быть вторым горизонтальным скроллером поверх сетки: иначе палец достаётся ему,
  // а он не может прокрутиться, и жест умирает, не дойдя до таблицы.
  expect(await grid.evaluate((node) => getComputedStyle(node.closest('.body')!).overflowX)).toBe('hidden');
  const touchAction = await grid.evaluate((node) => getComputedStyle(node).touchAction);
  expect(touchAction).toContain('pan-x');
  expect(touchAction).toContain('pan-y');

  const box = (await grid.boundingBox())!;
  const cdp = await page.context().newCDPSession(page);
  const y = box.y + Math.min(120, box.height / 2);
  await grid.evaluate((node) => { node.scrollLeft = 0; });
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: box.x + box.width - 30, y }] });
  for (let step = 1; step <= 12; step++) {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: box.x + box.width - 30 - step * 20, y }] });
  }
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await expect.poll(() => grid.evaluate((node) => node.scrollLeft)).toBeGreaterThan(100);

  // Шапка липнет сверху, но едет вбок вместе с колонками; первый столбец остаётся слева.
  await expect.poll(async () => {
    const header = (await grid.locator('thead th').nth(3).boundingBox())!;
    const cell = (await grid.locator('tbody tr[aria-rowindex]').first().locator('td').nth(2).boundingBox())!;
    return Math.abs(header.x - cell.x);
  }).toBeLessThanOrEqual(2);
  const first = (await grid.locator('tbody tr[aria-rowindex]').first().locator('th').boundingBox())!;
  expect(Math.abs(first.x - box.x)).toBeLessThanOrEqual(2);
});

test('an_owner_docx_opens_in_the_same_viewer_and_back_closes_it', async ({ page }) => {
  const { requested } = await boot(page);
  await openTile(page, page.locator('main'), 'Открыть документ Заметки.docx');
  const dialog = page.getByRole('dialog', { name: 'Заметки.docx' });
  await expect(dialog.locator('h1')).toHaveText('Отчёт о проверке сборки');
  expect(requested).toContain('owner-docx');
  await page.goBack();
  await expect(dialog).toHaveCount(0);
});

test('corrupt_legacy_and_locked_files_explain_themselves_and_offer_download', async ({ page }) => {
  const { share } = await boot(page);
  const card = page.locator(`[data-key="block:${share}"]`);
  const cases = [
    ['broken.docx', 'Не удалось прочитать файл', 'share-broken'],
    ['legacy.doc', 'Старый формат .doc здесь не открывается', 'share-legacy'],
    ['locked.docx', 'Файл защищён паролем', 'share-locked']
  ] as const;
  for (const [name, message, id] of cases) {
    await openTile(page, card, `Открыть документ ${name}`);
    const dialog = page.getByRole('dialog', { name });
    const failure = dialog.locator('[data-role="office-failure"]');
    await expect(failure).toBeVisible();
    await expect(failure.locator('p').first()).toHaveText(message);
    await expect(failure.locator('.hint')).toContainText('Скачайте');
    await expect(dialog.getByRole('link', { name: `Скачать ${name}` })).toHaveAttribute('href', `/api/v1/attachments/${id}/content`);
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
  }
});

test('a_file_over_ten_mebibytes_offers_only_download_without_reading_it', async ({ page }) => {
  const { share, requested } = await boot(page);
  const card = page.locator(`[data-key="block:${share}"]`);
  await openTile(page, card, 'Открыть таблицу huge.xlsx');
  const dialog = page.getByRole('dialog', { name: 'huge.xlsx' });
  await expect(dialog.locator('[data-role="office-failure"]')).toContainText('Файл большой для просмотра');
  await expect(dialog.getByRole('link', { name: 'Скачать huge.xlsx' })).toBeVisible();
  expect(requested).not.toContain('share-big');
});
