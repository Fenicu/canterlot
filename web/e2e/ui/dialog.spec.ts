import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { FIRST_DIALOG, FIRST_QUESTION, dialogConversation, playFirstDialog, seedFirstDialog } from '../../testing/fixtures/first-dialog';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function reloaded(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedFirstDialog(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('[data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

async function live(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = dialogConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await playFirstDialog(api, id);
  await expect(page.locator('[data-row]').last()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

const toolRow = (page: Page, text: string) => page.locator('.tool').filter({ has: page.locator('.head', { hasText: text }) });
const namedRow = (page: Page, name: string) => page.locator('.tool').filter({ has: page.locator('.name', { hasText: new RegExp(`^${name}$`) }) });

for (const [path, open] of [['after_reload', reloaded], ['live', live]] as const) {
  test.describe(path, () => {
    test('rows_read_as_the_mockup_and_no_internal_label_leaks', async ({ page }) => {
      await open(page);
      await expect(toolRow(page, 'canterlot_limits').locator('.name')).toHaveText('canterlot_limits');
      await expect(page.locator('.card header strong').last()).toHaveText('Claude');
      for (const leak of ['rchestrator', 'claude-opus-5-5', 'mcp__', '0,0 с', 'Bash: ', 'Read: ', 'Agent: ', 'инструмент']) {
        await expect(page.locator('.tool .head, .card header, .sub')).not.toContainText([leak]);
      }
      await expect(page.locator('.sub .title')).toHaveText('Субагент · Explore · Opus 5.5');
      await expect(page.locator('.sub .status')).toContainText('Find note.txt in check dir');
      const bash = toolRow(page, 'sleep 90').last();
      await expect(bash.locator('.name')).toHaveText('Bash');
      await expect(bash.locator('.summary')).toHaveText('sleep 90');
      await expect(toolRow(page, 'ToolSearch').locator('.summary')).toHaveCount(0);
      await expect(toolRow(page, 'AskUserQuestion').locator('.pill')).toHaveText('43 с');
    });

    test('empty_reasoning_takes_no_row_and_reasoning_with_text_stays', async ({ page }) => {
      await open(page);
      await expect(page.locator('.reasoning')).toHaveCount(2);
    });

    test('numbered_blocks_keep_their_numbers', async ({ page }) => {
      await open(page);
      await expect(page.locator('.prose ol[start="2"]')).toHaveCount(1);
      await expect(page.locator('.prose ol[start="3"]')).toHaveCount(1);
    });

    test('expanded_row_shows_the_real_output_and_a_true_line_count', async ({ page }) => {
      await open(page);
      const echo = toolRow(page, 'echo "Это длинная');
      await echo.locator('.head').click();
      await expect(echo.locator('.out pre')).toContainText('Это длинная проверочная строка');
      await expect(echo.locator('.out')).toContainText('1 строка');
      const read = namedRow(page, 'Read');
      await read.locator('.head').click();
      await expect(read.locator('.out pre')).toContainText('проверка кириллицы');
      await read.getByRole('button', { name: /Показать полностью/ }).click();
      await expect(read.locator('.out pre')).toContainText('проверка кириллицы');
      await expect(page.locator('[role="log"]')).not.toContainText('0 строк');
    });

    test('tool_without_output_says_so_instead_of_an_empty_panel', async ({ page }) => {
      await open(page);
      const search = toolRow(page, 'ToolSearch');
      await search.locator('.head').click();
      await expect(search.locator('.out')).toHaveText('Вывода нет');
      await expect(search.getByRole('button', { name: /Показать полностью/ })).toHaveCount(0);
    });

    test('failed_tool_is_named_and_its_wrapper_is_gone', async ({ page }) => {
      await open(page);
      const blocked = toolRow(page, 'sleep 90').first();
      await expect(blocked.locator('.pill')).toHaveText('Ошибка');
      await expect(blocked.locator('.pill')).toHaveAttribute('data-state', 'error');
      await blocked.locator('.head').click();
      await expect(blocked.locator('.out pre')).toContainText('Blocked: standalone sleep 90');
      await expect(blocked.locator('.out')).not.toContainText('tool_use_error');
    });

    test('owner_message_keeps_its_line_breaks', async ({ page }) => {
      await open(page);
      const bubble = page.locator('.bubble .text').filter({ hasText: 'Это проверка интерфейса' });
      await expect(bubble).toHaveCSS('white-space', 'pre-wrap');
      const height = await bubble.evaluate((node) => node.getBoundingClientRect().height);
      const line = await bubble.evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));
      expect(height).toBeGreaterThan(line * 10);
    });
  });
}

test('long_link_in_the_owner_message_wraps', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = dialogConversation(api);
  const text = `Смотри https://example.com/${'очень-длинный-путь/'.repeat(12)}\nи дальше`;
  api.histories.set(id, [{ kind: 'input', id: api.id(), idempotency_key: 'link', state: 'accepted', content: [{ type: 'text', text }], attachments: [], created_seq: '1', feed_seq: '1', created_at: '2026-10-31T10:20:52.858Z' }]);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  const bubble = page.locator('.bubble .text');
  await expect(bubble).toContainText('очень-длинный-путь');
  expect(await bubble.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true);
  expect(await page.locator('.feed').evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true);
});

test('reasoning_stays_away_until_it_has_text_and_an_open_one_does_not_jump', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = dialogConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const scope = `conversation:${id}`;
  const block_id = api.id();
  const started = { rev: 0, input_command_id: api.id(), parent_block_id: null, agent: 'orchestrator', model: 'claude-opus-5-5', tool_name: null, summary: null, approved_by: null };
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { ...started, block_id: api.id(), kind: 'text' } });
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { ...started, block_id, kind: 'reasoning' } });
  hub.flush();
  await expect(page.locator('.card')).toBeVisible();
  await expect(page.locator('.reasoning')).toHaveCount(0);
  hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id, base_rev: 0, rev: 1, append: 'Думаю ' } });
  hub.flush();
  const reasoning = page.locator('.reasoning');
  await expect(reasoning).toHaveCount(1);
  await reasoning.locator('.toggle').click();
  await expect(reasoning.locator('.fold')).toBeVisible();
  const before = await reasoning.locator('.toggle').boundingBox();
  hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id, base_rev: 1, rev: 2, append: 'дальше' } });
  hub.flush();
  await expect(reasoning.locator('.fold')).toContainText('Думаю дальше');
  await expect(reasoning.locator('.toggle')).toHaveAttribute('aria-expanded', 'true');
  expect(await reasoning.locator('.toggle').boundingBox()).toEqual(before);
});

test('list_marker_is_aligned_in_both_themes', async ({ page }) => {
  await reloaded(page);
  const item = page.locator('.prose ol[start="3"] li');
  await expect(item).toBeVisible();
  const left = await item.evaluate((node) => node.getBoundingClientRect().left);
  const text = await page.locator('.prose p').first().evaluate((node) => node.getBoundingClientRect().left);
  expect(left).toBeGreaterThan(text);
  await page.locator('[data-theme-toggle]').first().click();
  await expect(item).toHaveJSProperty('value', 0);
  expect(await item.evaluate((node) => node.getBoundingClientRect().left)).toBe(left);
  expect(await page.locator('.prose ol[start="3"]').evaluate((node) => (node as HTMLOListElement).start)).toBe(3);
});

/** Порядок детей карточки ответа: строка вызова, карточка запроса, следующий текст. */
async function feedOrder(page: Page, marks: string[]): Promise<number[]> {
  return page.locator('.card').last().evaluate((card, wanted) => {
    const nodes = [...card.children] as HTMLElement[];
    return wanted.map((mark) => nodes.findIndex((node) => node.innerText.includes(mark)));
  }, marks);
}

test('answered_question_stays_where_it_was_asked_live_and_after_reload', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = dialogConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await playFirstDialog(api, id, async (block) => {
    if (block.tool_name !== 'AskUserQuestion') return;
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: { request_id: FIRST_QUESTION.id, kind: 'question', payload: FIRST_QUESTION.request_payload }
    });
    hub.flush();
    const dock = page.locator('.request-cards-slot');
    await expect(dock).toContainText('Продолжать проверку интерфейса?');
    await expect(page.locator('[role="log"]')).not.toContainText('Продолжать проверку интерфейса?');
    await dock.getByText('Продолжить', { exact: true }).click();
    await dock.getByRole('button', { name: 'Ответить' }).click();
    await expect(page.locator('[role="log"]')).toContainText('Ответ: Продолжить');
    await expect(dock).toHaveCount(0);
  });
  const marks = ['AskUserQuestion', 'Ответ: Продолжить', 'Ответ получен'];
  await expect(page.locator('[role="log"]')).toContainText('Ответ получен');
  const live = await feedOrder(page, marks);
  expect(live[0]).toBeGreaterThanOrEqual(0);
  expect([...live].sort((a, b) => a - b)).toEqual(live);
  expect(new Set(live).size).toBe(3);
  api.histories.set(id, structuredClone(FIRST_DIALOG));
  await page.reload();
  await expect(page.locator('[role="log"]')).toContainText('Ответ: Продолжить');
  await expect(page.locator('.request-cards-slot')).toHaveCount(0);
  const reloaded = await feedOrder(page, marks);
  expect([...reloaded].sort((a, b) => a - b)).toEqual(reloaded);
  expect(new Set(reloaded).size).toBe(3);
});

test('decided_permission_stays_where_it_was_asked', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = dialogConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const scope = `conversation:${id}`;
  const run = api.id();
  const block = (kind: string, tool_name: string | null) => ({
    block_id: api.id(), rev: 0, kind, input_command_id: run, parent_block_id: null, agent: 'orchestrator', model: 'claude-opus-5-5', tool_name, summary: null, approved_by: null
  });
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: block('tool_call', 'Bash') });
  hub.append(scope, {
    kind: 'request_opened',
    conversation_id: id,
    payload: { request_id: api.id(), kind: 'permission', payload: { tool_name: 'Bash', input: { command: 'rm -rf build' } } }
  });
  hub.flush();
  const dock = page.locator('.request-cards-slot');
  await dock.getByRole('button', { name: 'Разрешить' }).click();
  await expect(page.locator('[role="log"]')).toContainText('Разрешено');
  await expect(dock).toHaveCount(0);
  const later = block('text', null);
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: later });
  hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: later.block_id, base_rev: 0, rev: 1, append: 'Сборка удалена' } });
  hub.flush();
  await expect(page.locator('[role="log"]')).toContainText('Сборка удалена');
  const order = await feedOrder(page, ['Bash', 'Разрешено', 'Сборка удалена']);
  expect([...order].sort((a, b) => a - b)).toEqual(order);
  expect(new Set(order).size).toBe(3);
});

test('header_is_flat_and_its_controls_line_up_with_the_content', async ({ page }) => {
  await reloaded(page);
  const bar = page.locator('.topbar');
  await expect(bar).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  await expect(bar).toHaveCSS('border-top-left-radius', '0px');
  const card = (await page.locator('article.card').first().boundingBox())!;
  const back = (await page.getByRole('link', { name: 'Все сессии' }).locator('svg').boundingBox())!;
  const theme = (await page.locator('[data-theme-toggle]').first().locator('svg').boundingBox())!;
  expect(Math.abs(back.x - card.x)).toBeLessThanOrEqual(1);
  expect(Math.abs(theme.x + theme.width - (card.x + card.width))).toBeLessThanOrEqual(1);
  for (const control of [page.getByRole('link', { name: 'Все сессии' }), page.getByRole('button', { name: 'Меню сессии' }), page.locator('[data-theme-toggle]').first()]) {
    const box = (await control.boundingBox())!;
    expect(box.width).toBeGreaterThanOrEqual(44);
    expect(box.height).toBeGreaterThanOrEqual(44);
  }
});
