import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

async function boot(page: import('@playwright/test').Page, count = 40) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  if (count > 0) {
    const items = seedHistory(api, id, count);
    // История сидится отдельно от журнала, и первое живое событие получает seq 1.
    // Номера 1..n встали бы в середину ленты и разрезали бы день пополам.
    for (let i = 0; i < items.length; i++) items[i]!.created_seq = String(i - items.length);
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  if (count > 0) await expect(page.locator('[data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

test('dom_is_bounded_with_10000_items_in_a_chat', async ({ page }) => {
  test.setTimeout(120_000);
  await boot(page, 10_000);
  const feed = page.locator('.feed');
  for (let step = 0; step < 40; step++) {
    await feed.evaluate((el) => { el.scrollTop = 0; });
    await page.waitForTimeout(80);
  }
  await expect.poll(async () => page.locator('[data-row]').count()).toBeLessThanOrEqual(80);
  expect(await page.evaluate(() => document.getElementsByTagName('*').length)).toBeLessThanOrEqual(6000);
});

test('memory_is_bounded_after_scrolling_through_10000_items', async ({ page }) => {
  test.setTimeout(120_000);
  await boot(page, 10_000);
  const feed = page.locator('.feed');
  for (let step = 0; step < 80; step++) {
    await feed.evaluate((el) => { el.scrollTop = 0; });
    if (await page.getByRole('button', { name: 'К последним сообщениям' }).isVisible()) break;
    await page.waitForTimeout(150);
  }
  await expect.poll(async () => Number(await feed.getAttribute('data-loaded'))).toBeLessThanOrEqual(3000);
  await expect(page.getByRole('button', { name: 'К последним сообщениям' })).toBeVisible();
});

test('older_page_loads_on_scroll_to_top_without_shifting_the_viewport', async ({ page }) => {
  await boot(page, 180);
  let release = (): void => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route('**/history**', async (route) => {
    await gate;
    await route.fallback();
  });
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.scrollTop = 0; });
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve(undefined)))));
  const row = page.locator('[data-row]').nth(1);
  const key = await row.getAttribute('data-key');
  const before = await row.boundingBox();
  await feed.evaluate((el) => { el.scrollTop = 0; });
  release();
  await expect.poll(async () => {
    const box = await page.locator(`[data-key="${key}"]`).boundingBox();
    return box && before ? Math.abs(box.y - before.y) : 99;
  }).toBeLessThanOrEqual(1);
});

test('reading_old_history_during_stream_keeps_the_anchor_within_one_pixel', async ({ page }) => {
  const { hub, api, id } = await boot(page, 80);
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.scrollTop = 40; });
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve(undefined)))));
  const key = await page.locator('[data-row]').nth(2).getAttribute('data-key');
  const before = await page.locator(`[data-key="${key}"]`).boundingBox();
  const blockId = api.id();
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  for (let rev = 1; rev <= 8; rev++) {
    hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: rev - 1, rev, append: 'ещё ' } });
  }
  hub.flush();
  await expect.poll(async () => {
    const box = await page.locator(`[data-key="${key}"]`).boundingBox();
    return box && before ? Math.abs(box.y - before.y) : 99;
  }).toBeLessThanOrEqual(1);
});

test('pinned_to_bottom_follows_the_stream_and_unpins_on_user_scroll', async ({ page }) => {
  const { hub, api, id } = await boot(page, 30);
  const feed = page.locator('.feed');
  await expect(feed).toHaveAttribute('data-pinned', '');
  const blockId = api.id();
  hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.append(`conversation:${id}`, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'хвост' } });
  hub.flush();
  await expect.poll(() => feed.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThanOrEqual(48);
  await feed.evaluate((el) => { el.scrollTop = Math.max(0, el.scrollTop - 200); });
  await expect(feed).not.toHaveAttribute('data-pinned', '');
});

test('new_messages_pill_appears_off_bottom_and_returns_to_pinned', async ({ page }) => {
  const { hub, api, id } = await boot(page, 30);
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.scrollTop = Math.max(0, el.scrollTop - 240); });
  const blockId = api.id();
  hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.flush();
  const pill = page.getByRole('button', { name: 'К новым сообщениям' });
  await expect(pill).toBeVisible();
  await pill.click();
  await expect(feed).toHaveAttribute('data-pinned', '');
});

test('expanding_a_tool_above_the_viewport_keeps_the_anchor', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const tool = toolBlock(api, 'tool-body\n'.repeat(30));
  const rest = Array.from({ length: 24 }, (_, index) => textBlock(api, `строка ${index}`, String(index + 2)));
  api.histories.set(id, [tool, ...rest]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible();
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.scrollTop = 280; });
  // Строка из запаса за окном может уйти из DOM при пересчёте высот; якорь проверяется по видимой строке.
  const key = await feed.evaluate((el) => {
    const box = el.getBoundingClientRect();
    const rows = [...el.querySelectorAll<HTMLElement>('[data-row]')].filter((row) => {
      const rect = row.getBoundingClientRect();
      return rect.top >= box.top && rect.bottom <= box.bottom;
    });
    return rows.at(-1)?.dataset.key ?? null;
  });
  expect(key).not.toBeNull();
  const before = await page.locator(`[data-key="${key}"]`).boundingBox();
  await page.locator('.head').evaluate((node) => (node as HTMLButtonElement).click());
  await expect.poll(async () => {
    const box = await page.locator(`[data-key="${key}"]`).boundingBox();
    return box && before ? Math.abs(box.y - before.y) : 99;
  }).toBeLessThanOrEqual(1);
});

test('keyboard_resize_keeps_a_pinned_chat_at_the_bottom', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 892 });
  await boot(page, 40);
  const feed = page.locator('.feed');
  await expect.poll(() => feed.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThanOrEqual(48);
  await page.setViewportSize({ width: 390, height: 560 });
  await expect.poll(() => feed.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThanOrEqual(48);
});

test('delta_batching_updates_dom_at_most_once_per_frame', async ({ page }) => {
  const { hub, api, id } = await boot(page, 1);
  const blockId = api.id();
  const scope = `conversation:${id}`;
  await page.evaluate(() => {
    const hits: number[] = [];
    let frame = 0;
    const stamp = () => { frame += 1; requestAnimationFrame(stamp); };
    requestAnimationFrame(stamp);
    const host = document.querySelector('.feed');
    new MutationObserver(() => { hits.push(frame); }).observe(host!, { subtree: true, characterData: true, childList: true });
    (window as unknown as { __hits?: number[] }).__hits = hits;
  });
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  for (let rev = 1; rev <= 1000; rev++) {
    hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: rev - 1, rev, append: 'x' } });
  }
  hub.flush();
  await page.waitForTimeout(300);
  const perFrame = await page.evaluate(() => {
    const hits = (window as unknown as { __hits?: number[] }).__hits ?? [];
    const counts = new Map<number, number>();
    for (const frame of hits) counts.set(frame, (counts.get(frame) ?? 0) + 1);
    return Math.max(0, ...counts.values());
  });
  expect(perFrame).toBeLessThanOrEqual(1);
});

test('render_budget_during_long_stream_has_no_long_tasks', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'chromium only');
  test.setTimeout(180_000);
  const { hub, api, id } = await boot(page, 1);
  const session = await page.context().newCDPSession(page);
  await session.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  await page.evaluate(() => {
    const tasks: number[] = [];
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) tasks.push(entry.duration);
    }).observe({ entryTypes: ['longtask'] });
    let frames = 0;
    const tick = () => { frames += 1; requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
    (window as unknown as { __tasks?: number[]; __frames?: () => number }).__tasks = tasks;
    (window as unknown as { __frames?: () => number }).__frames = () => frames;
  });
  const scope = `conversation:${id}`;
  const blockId = api.id();
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  for (let tool = 0; tool < 5; tool++) {
    hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { ...started(api.id()), kind: 'tool_call', tool_name: 'Read', summary: 'файл' } });
  }
  const startedAt = Date.now();
  let rev = 0;
  while (Date.now() - startedAt < 60_000) {
    for (let n = 0; n < 20; n++) {
      rev += 1;
      hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: rev - 1, rev, append: 'а' } });
    }
    hub.flush();
    await page.waitForTimeout(100);
  }
  const report = await page.evaluate(() => ({
    tasks: (window as unknown as { __tasks?: number[] }).__tasks ?? [],
    frames: (window as unknown as { __frames?: () => number }).__frames?.() ?? 0
  }));
  expect(report.tasks.some((duration) => duration >= 200)).toBe(false);
  expect(report.frames).toBeGreaterThan(100);
  expect(report.frames).toBeLessThan(20_000);
});

test('tool_blocks_are_collapsed_by_default_and_full_output_loads_lazily', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  api.histories.set(id, [toolBlock(api, 'строка\n'.repeat(8))]);
  api.bindConversation(id);
  const hits: string[] = [];
  page.on('request', (request) => { if (request.url().includes('/blocks/')) hits.push(request.url()); });
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible();
  await expect(page.locator('.out')).toHaveCount(0);
  expect(hits).toEqual([]);
  await page.locator('.head').click();
  expect(hits).toEqual([]);
  await page.getByRole('button', { name: /Показать полностью/ }).click();
  await expect.poll(() => hits.length).toBeGreaterThan(0);
});

test('pending_strip_shows_queue_and_scheduled_apart_from_the_live_line', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const when = new Date();
  when.setHours(23, 30, 0, 0);
  api.histories.set(id, [
    inputItem(api, 'q', 'queued', '1', 'черновик очереди'),
    textBlock(api, 'идёт ответ', '2', 'streaming')
  ]);
  api.bindConversation(id);
  const snap = api.conversationData(id);
  snap.scheduled = [{ id: api.id(), conversation_id: id, run_at: when.toISOString(), content: [{ type: 'text', text: 'позже' }], attachment_ids: [], state: 'pending', input_id: null, attempts: 0 }];
  api.hub.setSnapshot(`conversation:${id}`, () => snap);
  await page.reload();
  await expect(page.locator('.strip')).toContainText('В очереди');
  await expect(page.locator('.strip')).toContainText('Отправится в');
  await expect(page.locator('[role="log"]')).toContainText('Генерирует');
  await expect(page.locator('.strip')).not.toContainText('Генерирует');
});

test('streaming_status_is_announced_once_per_phase_not_per_token', async ({ page }) => {
  const { hub, api, id } = await boot(page, 1);
  const blockId = api.id();
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  for (let rev = 1; rev <= 12; rev++) {
    hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: rev - 1, rev, append: 'токен ' } });
  }
  hub.flush();
  await expect.poll(() => page.locator('.sr-only div', { hasText: 'Агент отвечает' }).count()).toBe(1);
});

test('long_code_scrolls_inside_the_block_at_360px', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const code = '```\n' + 'x'.repeat(240) + '\n```';
  api.histories.set(id, [textBlock(api, code, '1')]);
  api.bindConversation(id);
  await page.setViewportSize({ width: 360, height: 700 });
  await page.reload();
  await expect(page.locator('pre').first()).toBeVisible();
  const pageFits = await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);
  const codeScrolls = await page.locator('pre').first().evaluate((node) => node.scrollWidth > node.clientWidth);
  expect(pageFits).toBe(true);
  expect(codeScrolls).toBe(true);
});

test('subagent_row_links_to_the_lane_and_shows_live_status', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const call = textBlock(api, 'делегирую', '1');
  call.tool_name = 'Agent';
  call.block_kind = 'tool_call';
  call.summary = 'разобрать модуль';
  call.agent = 'ведущий';
  const child = textBlock(api, 'читаю', '2');
  child.parent_block_id = call.id;
  child.agent = 'поиск';
  child.model = 'haiku';
  child.tool_name = 'Read';
  child.summary = 'README';
  api.histories.set(id, [call, child]);
  api.bindConversation(id);
  await page.reload();
  const link = page.getByRole('link', { name: /Субагент/ });
  await expect(link).toHaveAttribute('href', `/c/${id}/lane/${call.id}`);
  await expect(link).toContainText('Читает');
});

test('user_bubble_marks_follow_input_state', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const states = ['queued', 'on_machine', 'transmitting', 'acceptance_unconfirmed', 'accepted', 'rejected'] as const;
  api.histories.set(id, states.map((state, index) => inputItem(api, state, state, String(index + 1), state)));
  api.bindConversation(id);
  await page.reload();
  for (const text of ['В очереди', 'На машине', 'Передаётся', 'Принято', 'доставлено', 'Отклонено']) {
    await expect(page.getByText(text, { exact: false }).first()).toBeVisible();
  }
});

test('incomplete_block_is_marked_and_partial_text_stays', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const block = textBlock(api, 'частичный текст', '1', 'incomplete');
  block.incomplete_reason = 'обрыв связи';
  api.histories.set(id, [block]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.getByText('частичный текст')).toBeVisible();
  await expect(page.getByText('Неполный ответ')).toBeVisible();
  await expect(page.getByText('обрыв связи')).toBeVisible();
});

test('error_and_warning_blocks_carry_text_not_colour_alone', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const warning = textBlock(api, 'мало места', '1');
  warning.block_kind = 'warning';
  const error = textBlock(api, 'сбор оборвался', '2');
  error.block_kind = 'error';
  api.histories.set(id, [warning, error]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.getByText('Предупреждение')).toBeVisible();
  await expect(page.getByText('мало места')).toBeVisible();
  await expect(page.getByText('Ошибка', { exact: true })).toBeVisible();
  await expect(page.getByText('сбор оборвался')).toBeVisible();
});

test('answer_card_header_shows_agent_time_model_and_info_button', async ({ page }) => {
  await boot(page, 1);
  await expect(page.locator('header')).toContainText('agent');
  await expect(page.locator('header')).toContainText('opus');
  await expect(page.getByRole('button', { name: 'Сведения о ходе' })).toBeVisible();
});

test('reduced_motion_stops_caret_and_expand_transitions', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  api.histories.set(id, [textBlock(api, 'поток', '1', 'streaming'), toolBlock(api, 'вывод')]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.locator('.caret')).toBeVisible();
  const caret = await page.locator('.caret').evaluate((node) => getComputedStyle(node).animationDuration);
  const expand = await page.locator('.head').evaluate((node) => getComputedStyle(node).transitionDuration);
  expect(parseFloat(caret)).toBe(0);
  expect(parseFloat(expand)).toBe(0);
});

test('feed_after_resync_shows_exact_text_without_a_jump_when_pinned', async ({ page }) => {
  const { hub, api, id } = await boot(page, 4);
  const feed = page.locator('.feed');
  const blockId = api.id();
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'точно' } });
  hub.flush();
  await expect(page.getByText('точно')).toBeVisible();
  const canonical = textBlock(api, 'точно', '9', 'final');
  canonical.id = blockId;
  api.blocks.set(blockId, { text: 'точно', rev: 1, state: 'final' });
  api.histories.set(id, [...(api.histories.get(id) ?? []), canonical]);
  const before = await feed.evaluate((el) => el.scrollTop);
  hub.chaos.forceResync('cursor_ahead');
  hub.flush();
  await expect(page.locator('.caret')).toHaveCount(0);
  await expect(page.getByText('точно')).toBeVisible();
  await expect.poll(() => feed.evaluate((el, top) => Math.abs(el.scrollTop - top), before)).toBeLessThanOrEqual(1);
});

test('tool_block_shows_thumbnail_without_layout_shift', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const tool = toolBlock(api, 'смотрит');
  tool.media = [{ media_id: api.id(), mime: 'image/png', width: 160, height: 90, ready: false }];
  api.histories.set(id, [tool]);
  api.bindConversation(id);
  await page.reload();
  const shot = page.locator('.shot');
  await expect(shot).toBeVisible();
  const before = await shot.boundingBox();
  await expect(shot).toContainText('картинка загружается');
  const after = await shot.boundingBox();
  expect(before && after ? Math.abs(before.height - after.height) : 99).toBeLessThanOrEqual(1);
});

test('media_placeholder_until_attached_event', async ({ page }) => {
  const { hub, api, id } = await boot(page, 0);
  const tool = toolBlock(api, 'смотрит');
  const mediaId = api.id();
  tool.media = [{ media_id: mediaId, mime: 'image/png', width: 120, height: 80, ready: false }];
  api.histories.set(id, [tool]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.getByText('картинка загружается')).toBeVisible();
  hub.append(`conversation:${id}`, {
    kind: 'block_media_attached',
    conversation_id: id,
    payload: { block_id: tool.id, media_id: mediaId, mime: 'image/png', size: 8, width: 120, height: 80 }
  });
  hub.flush();
  await expect(page.locator('.shot img')).toBeVisible();
  await expect(page.getByText('картинка загружается')).toHaveCount(0);
});

function started(blockId: string) {
  return {
    block_id: blockId,
    rev: 0,
    kind: 'text',
    input_command_id: null,
    parent_block_id: null,
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    approved_by: null
  };
}

function textBlock(api: FakeHubApi, content: string, seq: string, state = 'final'): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: content, rev: 1, state });
  return {
    kind: 'block', id, block_kind: 'text', content, content_bytes: content.length, content_truncated: false,
    created_seq: seq, created_at: AT, rev: 1, state, agent: 'агент', model: 'opus', tool_name: null, summary: null,
    parent_block_id: null, input_command_id: null, line_count: content.split('\n').length
  };
}

function toolBlock(api: FakeHubApi, content: string): HistoryBlock & { media?: { media_id: string; mime: string; width: number; height: number; ready: boolean }[] } {
  const block = textBlock(api, content, '1', 'final');
  block.block_kind = 'tool_call';
  block.tool_name = 'Read';
  block.summary = 'файл';
  block.line_count = content.split('\n').length;
  return block;
}

function inputItem(api: FakeHubApi, key: string, state: string, seq: string, text: string): HistoryInput {
  return {
    kind: 'input', id: api.id(), idempotency_key: key, state,
    content: [{ type: 'text', text }], attachments: [], created_seq: seq, feed_seq: seq, created_at: AT
  };
}
