import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { FIRST_DIALOG, FIRST_QUESTION, dialogConversation, playFirstDialog, seedFirstDialog, seedLongTurn } from '../../testing/fixtures/first-dialog';
import { sha256Hex } from '../../src/lib/stores/blocks';
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
      await expect(page.locator('.sub .status')).toHaveText('Готово · Find note.txt in check dir');
      await expect(page.locator('.sub .status')).toHaveAttribute('data-state', 'done');
      await expect(page.locator('.sub .pill')).toHaveText('3 с');
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

    test('complete_short_output_offers_nothing_more_and_wraps_inside_the_row', async ({ page }) => {
      await open(page);
      const echo = toolRow(page, 'echo "Это длинная');
      await echo.locator('.head').click();
      await expect(echo.locator('.out pre')).toContainText('Это длинная проверочная строка');
      await expect(echo.locator('.out')).not.toContainText('строка ·');
      await expect(echo.getByRole('button', { name: /Показать полностью/ })).toHaveCount(0);
      await expect(echo.locator('.out pre')).toHaveCSS('white-space', 'pre-wrap');
      expect(await echo.locator('.out pre').evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true);
      const read = namedRow(page, 'Read');
      await read.locator('.head').click();
      await expect(read.locator('.out pre')).toContainText('проверка кириллицы');
      await expect(read.getByRole('button', { name: /Показать полностью/ })).toHaveCount(0);
      await expect(page.locator('[role="log"]')).not.toContainText('0 строк');
    });

    test('tool_without_output_says_so_instead_of_an_empty_panel', async ({ page }) => {
      await open(page);
      const search = toolRow(page, 'ToolSearch');
      await search.locator('.head').click();
      await expect(search.locator('.out')).toHaveText('Вывода нет');
      await expect(search.getByRole('button', { name: /Показать полностью/ })).toHaveCount(0);
    });

    test('cli_note_about_an_empty_output_reads_as_no_output', async ({ page }) => {
      await open(page);
      const mkdir = toolRow(page, 'mkdir -p /tmp/canterlot-check');
      await mkdir.locator('.head').click();
      await expect(mkdir.locator('.out')).toHaveText('Вывода нет');
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

async function longTurn(page: Page, steps: number, shape: 'steps' | 'text_between') {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const turn = seedLongTurn(api, steps, shape);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${turn.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.card').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, ...turn };
}

/** Что видно сверху окна: ключ первой строки карточки и её положение. */
const topRow = (page: Page) => page.locator('.feed').evaluate((feed) => {
  const edge = feed.getBoundingClientRect().top;
  const rows = [...feed.querySelectorAll<HTMLElement>('.card [data-key]')];
  const row = rows.find((item) => item.getBoundingClientRect().top >= edge);
  return row ? { key: row.dataset.key ?? '', y: row.getBoundingClientRect().top } : null;
});

const shownKeys = (page: Page) => page.locator('.feed').evaluate((feed) => [...feed.querySelectorAll<HTMLElement>('.card [data-key]')].map((row) => row.dataset.key ?? ''));

test('reading_a_long_turn_up_and_down_loses_no_row_and_keeps_the_anchor', async ({ page }) => {
  test.setTimeout(60_000);
  await longTurn(page, 96, 'text_between');
  const feed = page.locator('.feed');
  await expect(feed).toHaveAttribute('data-loaded', '100');
  await page.locator('.card').first().evaluate((card) => { (card as HTMLElement & { mark?: boolean }).mark = true; });

  // Первый экран над разрезом: эти строки владелец читает, пока грузится старшая страница.
  await feed.evaluate((el) => { el.scrollTop = el.scrollHeight - el.clientHeight - 1400; });
  await expect(feed).not.toHaveAttribute('data-pinned', '');
  const head = await shownKeys(page);
  for (let pass = 0; pass < 30 && (await feed.getAttribute('data-loaded')) !== '291'; pass++) {
    await feed.evaluate((el) => { el.scrollTop = 0; });
    await page.waitForTimeout(60);
    const anchor = await topRow(page);
    expect(anchor).not.toBeNull();
    await page.waitForTimeout(160);
    await expect.poll(async () => {
      const box = await page.locator(`[data-key="${anchor?.key}"]`).boundingBox();
      return Math.abs((box?.y ?? -9999) - (anchor?.y ?? 0));
    }).toBeLessThanOrEqual(1);
  }
  await expect(feed).toHaveAttribute('data-loaded', '291');
  const after = await shownKeys(page);
  expect(head.filter((key) => !after.includes(key))).toEqual([]);
  expect(await page.locator('.card').first().evaluate((card) => (card as HTMLElement & { mark?: boolean }).mark)).toBe(true);
  await expect(page.getByRole('button', { name: 'К новым сообщениям' })).toHaveCount(0);

  await feed.evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await expect(page.getByRole('button', { name: 'К новым сообщениям' })).toHaveCount(0);
  await feed.evaluate((el) => { el.scrollTop = 0; });
  expect((await shownKeys(page)).length).toBe(after.length);
});

test('the_restore_button_stands_at_the_cut_and_opens_rows_in_place', async ({ page }) => {
  const { items } = await longTurn(page, 60, 'text_between');
  const seam = page.getByRole('button', { name: /Показать ещё \d+ шаг/ });
  await expect(seam).toBeAttached();
  const place = await seam.evaluate((button) => {
    const row = (node: Element | null) => node?.querySelector<HTMLElement>('[data-key]')?.dataset.key ?? (node as HTMLElement | null)?.dataset.key ?? '';
    return { before: row(button.previousElementSibling), after: row(button.nextElementSibling) };
  });
  const order = items.flatMap((item) => (item.kind === 'block' && !item.parent_block_id ? [`block:${item.id}`] : []));
  expect(place.before).not.toBe('');
  expect(order.indexOf(place.after)).toBeGreaterThan(order.indexOf(place.before) + 1);
  await seam.scrollIntoViewIfNeeded();
  const spot = (await seam.boundingBox())?.y ?? 0;
  await seam.click();
  await expect(seam).toHaveCount(0);
  const opened = await page.locator(`[data-key="${order[order.indexOf(place.before) + 1]}"]`).boundingBox();
  expect(Math.abs((opened?.y ?? -9999) - spot)).toBeLessThanOrEqual(20);
});

test('tail_growth_and_an_opened_row_do_not_move_what_the_reader_is_looking_at', async ({ page }) => {
  const { hub, api, id, items } = await longTurn(page, 30, 'text_between');
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.scrollTop = el.scrollHeight - el.clientHeight - 700; });
  await expect(feed).not.toHaveAttribute('data-pinned', '');
  await page.waitForTimeout(100);
  const anchor = await topRow(page);
  const run = (items.at(-1) as { input_command_id: string }).input_command_id;
  const scope = `conversation:${id}`;
  for (let extra = 0; extra < 3; extra++) {
    const block_id = api.id();
    hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { block_id, rev: 0, kind: 'text', input_command_id: run, parent_block_id: null, agent: 'orchestrator', model: 'claude-opus-5-5', tool_name: null, summary: null, approved_by: null } });
    hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id, base_rev: 0, rev: 1, append: 'Новая строка хвоста\n\n'.repeat(6) } });
    hub.flush();
  }
  await expect(page.getByRole('button', { name: 'К новым сообщениям' })).toBeVisible();
  const held = await page.locator(`[data-key="${anchor?.key}"]`).boundingBox();
  expect(Math.abs((held?.y ?? -9999) - (anchor?.y ?? 0))).toBeLessThanOrEqual(1);
  const row = page.locator('.tool .head').filter({ hasText: 'Read' }).last();
  await row.scrollIntoViewIfNeeded();
  await page.waitForTimeout(100);
  const before = await row.boundingBox();
  await row.click();
  await page.waitForTimeout(300);
  const after = await row.boundingBox();
  expect(Math.abs((after?.y ?? -9999) - (before?.y ?? 0))).toBeLessThanOrEqual(1);
});

test('long_turn_folds_into_one_row_and_the_final_answer_stays_outside', async ({ page }) => {
  await longTurn(page, 58, 'steps');
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.scrollTop = 0; });
  await expect(feed).toHaveAttribute('data-loaded', '134');
  await feed.evaluate((el) => { el.scrollTop = 0; });
  const group = page.locator('.steps');
  await expect(group).toHaveCount(1);
  await expect(group.locator('.surface .name')).toHaveText('58 шагов');
  await expect(group.locator('.surface .summary')).toHaveText('Bash 44 · Read 14');
  await expect(group.locator('.surface .pill')).toHaveText('3 мин');
  await expect(page.locator('.tool')).toHaveCount(0);
  await expect(page.locator('.reasoning')).toHaveCount(0);
  await expect(page.locator('.prose')).toHaveCount(2);
  const order = await page.locator('.card > .entry').evaluateAll((rows) => rows.map((row) => (row.querySelector('.steps') ? 'group' : 'text')));
  expect(order).toEqual(['text', 'group', 'text']);
  const box = (await group.boundingBox())!;
  await group.locator('.surface .head').click();
  await expect(group.getByRole('button', { name: /Показать ещё \d+ шаг/ })).toBeVisible();
  await expect(page.locator('.tool').first()).toBeVisible();
  expect(Math.abs((await group.boundingBox())!.y - box.y)).toBeLessThanOrEqual(1);
  await group.getByRole('button', { name: /Показать ещё/ }).click();
  await expect(group.locator('.reasoning')).toHaveCount(15);
  await expect(group.locator('.tool')).toHaveCount(58);
  await group.locator('.surface .head').click();
  await expect(page.locator('.tool')).toHaveCount(0);
});

test('failed_step_shows_on_the_summary_and_inside_the_opened_group', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const turn = seedLongTurn(api, 6, 'steps');
  const result = turn.items.filter((item) => item.kind === 'block' && item.block_kind === 'tool_result')[2] as { summary: string | null };
  result.summary = 'error';
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${turn.id}`);
  const group = page.locator('.steps');
  await expect(group.locator('.surface .pill')).toHaveText('Ошибка 1');
  await expect(group.locator('.surface .pill')).toHaveAttribute('data-state', 'error');
  await group.locator('.surface .head').click();
  await expect(group.locator('.tool .pill[data-state="error"]')).toHaveCount(1);
});

test('live_turn_folds_steps_without_a_jump_and_keeps_the_running_one_and_the_owner_choice', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = dialogConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const scope = `conversation:${id}`;
  const run = api.id();
  const at = (seconds: number) => new Date(Date.parse('2026-10-31T10:30:00.000Z') + seconds * 1000).toISOString();
  const start = (block_id: string, kind: string, tool_name: string | null, parent: string | null, when: string) =>
    hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { block_id, rev: 0, kind, input_command_id: run, parent_block_id: parent, agent: 'orchestrator', model: 'claude-opus-5-5', tool_name, summary: tool_name ? `${tool_name}: команда` : null, approved_by: null }, at: when });
  const finish = async (block_id: string, text: string, when: string) =>
    hub.append(scope, { kind: 'block_final', conversation_id: id, payload: { block_id, rev: 1, text, checksum: await sha256Hex(text), text_omitted: false, summary: null }, at: when });
  hub.append(scope, { kind: 'derived_changed', conversation_id: id, payload: { conversation_id: id, activity: { state: 'working', evidence: { phase: 'running_tool' }, since: at(0) }, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: at(0), settings: { model: 'opus-5.5', effort: 'high', permission_mode: 'owner' }, crunch: { enabled: false } } });
  const step = async (index: number) => {
    const call = api.id();
    start(call, 'tool_call', 'Bash', null, at(index * 2));
    await finish(call, '', at(index * 2));
    hub.flush();
    await expect(page.locator('.tool .pill[data-state="running"]')).toHaveCount(1);
    return call;
  };
  const done = async (call: string, index: number) => {
    const result = api.id();
    start(result, 'tool_result', null, call, at(index * 2 + 1));
    await finish(result, 'ok', at(index * 2 + 1));
    hub.flush();
    await expect(page.locator('.tool .pill[data-state="running"]')).toHaveCount(0);
  };
  const feed = page.locator('.feed');
  const bottom = () => feed.evaluate((el) => el.scrollHeight);
  await done(await step(0), 0);
  await done(await step(1), 1);
  const heights: number[] = [];
  const third = await step(2);
  await expect(page.locator('.steps .surface .name')).toHaveText('2 шага');
  await page.locator('.steps').evaluate((node) => { (node as HTMLElement & { mark?: boolean }).mark = true; });
  heights.push(await bottom());
  await page.locator('.steps .surface .head').click();
  await expect(page.locator('.steps .tool')).toHaveCount(2);
  await page.locator('.steps .surface .head').click();
  await expect(page.locator('.steps .tool')).toHaveCount(0);
  await done(third, 2);
  for (let index = 3; index < 6; index++) {
    const previous = page.locator('.card > .entry .tool');
    await expect(previous).toHaveCount(1);
    await done(await step(index), index);
    await expect(page.locator('.steps .surface .name')).toHaveText(`${index} шага`.replace('5 шага', '5 шагов'));
    await expect(page.locator('.card > .entry .tool')).toHaveCount(1);
    heights.push(await bottom());
  }
  expect(new Set(heights).size).toBe(1);
  expect(await page.locator('.steps').evaluate((node) => (node as HTMLElement & { mark?: boolean }).mark)).toBe(true);
  await page.locator('.steps .surface .head').click();
  await expect(page.locator('.steps .tool')).toHaveCount(5);
  await done(await step(6), 6);
  await expect(page.locator('.steps .surface .head')).toHaveAttribute('aria-expanded', 'true');
  await expect(page.locator('.steps .tool')).toHaveCount(6);
});
