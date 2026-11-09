import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { sha256Hex } from '../../src/lib/stores/blocks';
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

/** Замер, переставший меняться между двумя чтениями: прокрутка и виртуализатор догоняют раскладку не в тот же кадр. */
async function settled<T>(read: () => Promise<T>, same: (a: T, b: T) => boolean): Promise<T> {
  let last = await read();
  await expect.poll(async () => {
    const next = await read();
    const still = same(next, last);
    last = next;
    return still;
  }).toBe(true);
  return last;
}

/**
 * Ключ и верх n-й строки окна, прочитанные после кадра одним вызовом. Событие прокрутки
 * перестраивает окно только в следующем кадре: чтение до него берёт строку прежнего окна,
 * которая тут же уходит из DOM, и два таких чтения в одном кадре сходятся на ней.
 */
async function rowAt(page: import('@playwright/test').Page, n: number): Promise<{ key: string; y: number }> {
  return page.locator('.feed').evaluate((el, n) => new Promise<{ key: string; y: number }>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const row = el.querySelectorAll<HTMLElement>('[data-row]')[n];
      resolve({ key: row?.dataset.key ?? '', y: row ? row.getBoundingClientRect().top : Number.NaN });
    }));
  }), n);
}

async function rowAnchor(page: import('@playwright/test').Page, n: number): Promise<{ key: string; y: number }> {
  return settled(() => rowAt(page, n), (a, b) => a.key !== '' && a.key === b.key && Math.abs(a.y - b.y) <= 0.5);
}

/** Сдвиг строки от замера; строки нет в DOM — 99, а не ожидание её возвращения. */
async function drift(page: import('@playwright/test').Page, anchor: { key: string; y: number }): Promise<number> {
  const top = await page.locator('.feed').evaluate((el, key) => {
    const node = [...el.querySelectorAll<HTMLElement>('[data-key]')].find((item) => item.dataset.key === key);
    return node ? node.getBoundingClientRect().top : null;
  }, anchor.key);
  return top === null ? 99 : Math.abs(top - anchor.y);
}

test('dom_is_bounded_with_10000_items_in_a_chat', async ({ page }) => {
  test.setTimeout(120_000);
  await boot(page, 10_000);
  const feed = page.locator('.feed');
  for (let step = 0; step < 40; step++) {
    await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 0; });
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
    await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 0; });
    if (await page.getByRole('button', { name: 'К последним сообщениям' }).isVisible()) break;
    await page.waitForTimeout(150);
  }
  await expect.poll(async () => Number(await feed.getAttribute('data-loaded'))).toBeLessThanOrEqual(3000);
  await expect(page.getByRole('button', { name: 'К последним сообщениям' })).toBeVisible();
});

async function detachTail(page: import('@playwright/test').Page): Promise<void> {
  const feed = page.locator('.feed');
  const jump = page.getByRole('button', { name: 'К последним сообщениям' });
  for (let step = 0; step < 120; step++) {
    await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 0; });
    if (await jump.isVisible()) return;
    await page.waitForTimeout(100);
  }
  await expect(jump).toBeVisible();
}

test('detached_window_stays_live_and_the_jump_button_returns_to_the_tail', async ({ page }) => {
  test.setTimeout(120_000);
  const { hub, api, id } = await boot(page, 3200);
  await detachTail(page);
  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: api.id() } } } });
  const blockId = api.id();
  const opened = hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'свежий ответ' } });
  // Настоящий хаб пишет блок в историю вместе с событием.
  api.histories.get(id)!.push({ ...textBlock(api, 'свежий ответ', opened.scope_seq, 'streaming'), id: blockId });
  api.blocks.set(blockId, { text: 'свежий ответ', rev: 1, state: 'streaming' });
  hub.flush();
  // Состояние хода живёт, пока читают старое: пункт прерывания в меню сессии есть сразу.
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await expect(page.getByRole('menuitem', { name: 'Прервать ход' })).toBeVisible();
  await page.keyboard.press('Escape');
  const jump = page.getByRole('button', { name: 'К последним сообщениям' });
  await jump.click();
  await expect(jump).toBeHidden();
  await expect(page.getByText('свежий ответ')).toBeVisible();
  await expect(page.locator('.feed')).toHaveAttribute('data-pinned', '');
});

test('scrolling_down_a_detached_window_loads_newer_pages_back_to_the_tail', async ({ page }) => {
  test.setTimeout(120_000);
  await boot(page, 3200);
  await detachTail(page);
  const feed = page.locator('.feed');
  const jump = page.getByRole('button', { name: 'К последним сообщениям' });
  for (let step = 0; step < 120 && (await jump.isVisible()); step++) {
    await feed.evaluate((el) => { el.scrollTop = el.scrollHeight; });
    await page.waitForTimeout(100);
  }
  await expect(jump).toBeHidden();
  await expect.poll(async () => Number(await feed.getAttribute('data-loaded'))).toBeLessThanOrEqual(3000);
  await expect(page.locator('[data-row]').last()).toContainText('3200');
});

test('reading_the_tail_marks_the_chat_read_and_reading_old_history_does_not', async ({ page }) => {
  const { hub, api, id } = await boot(page, 0);
  const reads: string[] = [];
  await page.route('**/api/v1/conversations/*/read', async (route) => {
    reads.push(String((route.request().postDataJSON() as { seq?: unknown }).seq));
    await route.fallback();
  });
  api.histories.set(id, Array.from({ length: 30 }, (_, index) => textBlock(api, `строка ${index}`, String(index + 1))));
  // Журнал хаба идёт вровень с историей: живое событие получит номер после неё.
  for (let n = 0; n < 30; n++) hub.append(`conversation:${id}`, { kind: 'phase_changed', conversation_id: id, payload: { phase: 'generating', tool_name: null, summary: null } });
  api.bindConversation(id);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible();
  await expect.poll(() => reads.at(-1)).toBe('30');
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 0; });
  await expect(feed).not.toHaveAttribute('data-pinned', '');
  const blockId = api.id();
  const opened = hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.flush();
  await page.waitForTimeout(2500);
  expect(reads.at(-1)).toBe('30');
  await page.getByRole('button', { name: 'К новым сообщениям' }).click();
  await expect.poll(() => reads.at(-1), { timeout: 5000 }).toBe(opened.scope_seq);
});

// QUARANTINE: подгрузка не укладывается в 5 с под нагрузкой (1 раз при нагрузке 40; 8 из 8 отдельно)
test.describe('quarantine older_page_loads_on_scroll_to_top_without_shifting_the_viewport', () => {
  test.describe.configure({ retries: 2 });
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
    await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 0; });
    const before = await rowAnchor(page, 1);
    await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 0; });
    release();
    await expect.poll(() => drift(page, before)).toBeLessThanOrEqual(1);
  });
});

test('reading_old_history_during_stream_keeps_the_anchor_within_one_pixel', async ({ page }) => {
  const { hub, api, id } = await boot(page, 80);
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 40; });
  const before = await rowAnchor(page, 2);
  const blockId = api.id();
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  for (let rev = 1; rev <= 8; rev++) {
    hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: rev - 1, rev, append: 'ещё ' } });
  }
  hub.flush();
  await expect.poll(() => drift(page, before)).toBeLessThanOrEqual(1);
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
  // Уход вверх — жест владельца: колесо приходит раньше сдвига.
  await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -200 })); el.scrollTop = Math.max(0, el.scrollTop - 200); });
  await expect(feed).not.toHaveAttribute('data-pinned', '');
});

test('new_messages_pill_appears_off_bottom_and_returns_to_pinned', async ({ page }) => {
  const { hub, api, id } = await boot(page, 30);
  const feed = page.locator('.feed');
  await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = Math.max(0, el.scrollTop - 240); });
  // Событие, пришедшее раньше отрыва от низа, лента просто догонит, и плашка не появится.
  await expect(feed).not.toHaveAttribute('data-pinned', '');
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
  await feed.evaluate((el) => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); el.scrollTop = 280; });
  // Строка из запаса за окном может уйти из DOM при пересчёте высот; якорь проверяется по видимой строке.
  // Выбор делается по устоявшейся раскладке: прокрутка и виртуализатор перемеряют строки не в тот же кадр.
  const anchor = () => feed.evaluate((el) => {
    const box = el.getBoundingClientRect();
    const rows = [...el.querySelectorAll<HTMLElement>('[data-row]')].filter((row) => {
      const rect = row.getBoundingClientRect();
      return rect.top >= box.top && rect.bottom <= box.bottom;
    });
    const last = rows.at(-1);
    return last ? { key: last.dataset.key!, y: last.getBoundingClientRect().top } : null;
  });
  let picked = await anchor();
  await expect.poll(async () => {
    const next = await anchor();
    const stable = next !== null && picked !== null && next.key === picked.key && Math.abs(next.y - picked.y) <= 0.5;
    picked = next;
    return stable;
  }).toBe(true);
  expect(picked).not.toBeNull();
  const key = picked!.key;
  const before = picked!.y;
  await page.locator('.head').evaluate((node) => (node as HTMLButtonElement).click());
  // Раскрытие выше окна сдвигает высоты строк: якорь сверяется, когда переход доиграл и лента перемерила строки.
  await expect.poll(() => page.locator('.fold').first().evaluate((el) => el.getAnimations().length)).toBe(0);
  // Прокрутка в Firefox целочисленная, а высоты строк дробные: восстановленный якорь может встать
  // на пиксель прокрутки плюс дробный остаток раскладки (до 1.8 px). Прыжок здесь — это десятки пикселей.
  await expect.poll(() => page.evaluate(({ key, before }) => {
    const el = document.querySelector<HTMLElement>(`[data-key="${key}"]`);
    return el ? Math.abs(el.getBoundingClientRect().top - before) : 99;
  }, { key, before }), { timeout: 15_000 }).toBeLessThanOrEqual(2);
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
    // Часы строки «Генерирует · N с» идут по своему секундному таймеру, а не по дельтам: под нагрузкой
    // кадр длиннее, и их тик попадает в один кадр с пачкой дельт. Считаются только остальные изменения.
    const clock = (node: Node) => (node instanceof Element ? node : node.parentElement)?.closest('.live') !== null;
    new MutationObserver((records) => {
      if (records.some((record) => !clock(record.target))) hits.push(frame);
    }).observe(host!, { subtree: true, characterData: true, childList: true });
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
  // Пачка дельт даёт не больше одной перерисовки; второй вызов в кадре — шаг проявления текста.
  expect(perFrame).toBeLessThanOrEqual(2);
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
  const tool = toolBlock(api, 'строка\n'.repeat(8));
  tool.content_truncated = true;
  api.histories.set(id, [tool]);
  api.bindConversation(id);
  const hits: string[] = [];
  page.on('request', (request) => { if (request.url().includes('/blocks/')) hits.push(request.url()); });
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible();
  await expect(page.locator('.out')).toHaveCount(0);
  expect(hits).toEqual([]);
  await page.locator('.head').click();
  expect(hits).toEqual([]);
  // Кнопка не должна нажиматься в движении: пока раскрытие доигрывает, лента перемеряет высоту
  // строки и подтягивает прокрутку, и нажатие теряется между mousedown и mouseup.
  await expect.poll(() => page.locator('.fold').first().evaluate((el) => el.getAnimations().length)).toBe(0);
  await page.getByRole('button', { name: /Показать полностью/ }).click();
  await expect.poll(() => hits.length).toBeGreaterThan(0);
});

test('full_output_action_appears_only_when_there_is_more_to_show', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const long = toolBlock(api, Array.from({ length: 30 }, (_, line) => `строка ${line}`).join('\n'));
  long.line_count = 30;
  api.histories.set(id, [long]);
  api.bindConversation(id);
  await page.reload();
  await page.locator('.head').click();
  await expect(page.getByRole('button', { name: 'Показать полностью · 30 строк' })).toBeVisible();
  await page.getByRole('button', { name: /Показать полностью/ }).click();
  await expect(page.locator('.out pre')).toContainText('строка 0');
  await expect(page.getByRole('button', { name: /Показать полностью/ })).toHaveCount(0);
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
  // Широкий код листается и с клавиатуры: блок — именованная область в порядке Tab.
  const pre = page.getByRole('region', { name: 'Код' }).first();
  await expect(pre).toHaveAttribute('tabindex', '0');
  await pre.focus();
  await page.keyboard.press('ArrowRight');
  await expect.poll(() => pre.evaluate((node) => node.scrollLeft)).toBeGreaterThan(0);
});

test('quote_with_a_long_code_line_stays_inside_the_card_and_both_copy_buttons_work', async ({ page }, testInfo) => {
  const command = `Write-Host "${'x'.repeat(400)}"`;
  const quote = [
    '> ## Обновление сборки mc',
    '>',
    '> Друзья, **обновляем** сборку `mc`:',
    '>',
    '> 1. Скачайте архив',
    '> 2. Выполните в PowerShell:',
    '>    ```powershell',
    `>    ${command}`,
    '>    ```',
    '> 3. Готово',
    '>',
    '> > Вложенная цитата'
  ].join('\n');
  const inner = [
    '## Обновление сборки mc',
    '',
    'Друзья, **обновляем** сборку `mc`:',
    '',
    '1. Скачайте архив',
    '2. Выполните в PowerShell:',
    '   ```powershell',
    `   ${command}`,
    '   ```',
    '3. Готово',
    '',
    '> Вложенная цитата'
  ].join('\n');
  const { api, id } = await boot(page, 0);
  api.histories.set(id, [textBlock(api, quote, '1')]);
  api.bindConversation(id);
  await page.reload();
  const card = page.locator('article.card').last();
  const pre = card.locator('pre').first();
  await expect(pre).toBeVisible();
  const phone = testInfo.project.name === 'phone';
  const codeButton = card.getByRole('button', { name: 'Скопировать код' });
  const quoteButton = card.getByRole('button', { name: 'Скопировать цитату' });
  await expect(codeButton).toBeVisible();
  await expect(quoteButton).toBeVisible();

  const cardBox = await card.boundingBox();
  const preBox = await pre.boundingBox();
  expect(preBox!.x + preBox!.width).toBeLessThanOrEqual(cardBox!.x + cardBox!.width + 1);
  expect(await card.evaluate((node) => node.scrollWidth - node.clientWidth)).toBeLessThanOrEqual(0);
  expect(await pre.evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);

  if (phone) {
    await expect(codeButton).toHaveCSS('opacity', '1');
  } else {
    await expect(codeButton).toHaveCSS('opacity', '0');
    await card.locator('.code-block').first().hover();
    await expect(codeButton).toHaveCSS('opacity', '1');
  }

  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: async (text: string) => { (window as unknown as { __copied?: string }).__copied = text; } }
    });
  });
  const copied = () => page.evaluate(() => (window as unknown as { __copied?: string }).__copied);

  await codeButton.click();
  await expect.poll(copied).toBe(`${command}\n`);
  await expect(codeButton).toContainText('Скопировано');
  await expect(codeButton).not.toContainText('Скопировано', { timeout: 4000 });

  if (!phone) await card.locator('blockquote').first().hover();
  await quoteButton.click();
  await expect.poll(copied).toBe(inner);
  await expect(quoteButton).toContainText('Скопировано');

  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: async () => { throw new Error('denied'); } }
    });
  });
  await quoteButton.click();
  await expect(quoteButton).toContainText('Не удалось скопировать');
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
  const run = api.id();
  call.input_command_id = run;
  child.input_command_id = run;
  api.histories.set(id, [call, child]);
  api.bindConversation(id);
  const detail = api.details.get(id);
  if (detail) detail.activity = { state: 'working', evidence: { phase: 'running_tool' }, since: AT };
  await page.reload();
  const link = page.getByRole('link', { name: /Субагент/ });
  await expect(link).toHaveAttribute('href', `/c/${id}/lane/${call.id}`);
  await expect(link).toContainText('Читает');
  await expect(link.locator('.status')).toHaveAttribute('data-state', 'running');
});

function backgroundTask(id: string, blockId: string, kind: string, state: string) {
  return {
    task_id: id, kind, block_id: blockId, parent_task_id: null, description: 'фоном', state,
    started_at: AT, ended_at: state === 'running' ? null : '2026-10-28T12:01:30.000Z'
  };
}

function launchResult(api: FakeHubApi, parent: string, seq: string, text: string): HistoryBlock {
  const block = textBlock(api, text, seq);
  block.block_kind = 'tool_result';
  block.parent_block_id = parent;
  block.summary = 'async_launched';
  block.agent = 'orchestrator';
  return block;
}

test('background_subagent_works_until_its_task_ends_and_the_header_counts_it', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const call = textBlock(api, '{}', '1');
  call.tool_name = 'Agent';
  call.block_kind = 'tool_call';
  call.summary = 'разобрать модуль';
  call.agent = 'orchestrator';
  api.histories.set(id, [call, launchResult(api, call.id, '2', 'Async agent launched successfully.')]);
  api.bindConversation(id);
  const detail = api.details.get(id);
  if (detail) {
    detail.activity = { state: 'idle', evidence: { kind: 'idle', last_turn_at: AT, background_work: 1 }, since: AT };
    detail.execution = { state: 'running' };
  }
  api.pushBackground(id, [backgroundTask('a1', call.id, 'agent', 'running')]);
  await page.reload();
  const link = page.getByRole('link', { name: /Субагент/ });
  await expect(link.locator('.status')).toHaveAttribute('data-state', 'running');
  await expect(link.locator('.status')).toContainText('Работает');
  const state = page.locator('.state-chip');
  await expect(state).toHaveText('Ожидает сообщения · 1 фоновая');
  await expect(state).toHaveClass(/live/);
  await expect(state).not.toHaveClass(/sr-only/);
  await expect(page.locator('form.composer textarea, .composer textarea').first()).toBeEnabled();

  api.pushBackground(id, [backgroundTask('a1', call.id, 'agent', 'completed')]);
  await expect(link.locator('.status')).toHaveAttribute('data-state', 'done');
  await expect(link.locator('.status')).toContainText('Готово');
});

test('background_command_is_a_plain_row_with_a_badge_and_runs_until_its_task_ends', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const call = toolBlock(api, 'sleep 25');
  call.tool_name = 'Bash';
  call.summary = 'Bash: sleep 25';
  api.histories.set(id, [call, launchResult(api, call.id, '2', 'Command running in background with ID: b1')]);
  api.bindConversation(id);
  const detail = api.details.get(id);
  if (detail) detail.execution = { state: 'running' };
  api.pushBackground(id, [backgroundTask('b1', call.id, 'bash', 'running')]);
  await page.reload();
  const row = page.locator('.tool').filter({ hasText: 'sleep 25' });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText('Фоном')).toBeVisible();
  await expect(row.locator('[data-state="running"]')).toBeVisible();
  await expect(page.getByRole('link', { name: /Субагент/ })).toHaveCount(0);

  api.pushBackground(id, [backgroundTask('b1', call.id, 'bash', 'failed')]);
  await expect(row.locator('[data-state="error"]')).toBeVisible();
  await expect(row.getByText('Фоном')).toBeVisible();

  api.pushBackground(id, [backgroundTask('b1', call.id, 'bash', 'stopped')]);
  await expect(row.getByText('Прервано')).toBeVisible();
  await expect(row.locator('[data-state="error"]')).toHaveCount(0);
});

test('user_bubble_marks_follow_input_state', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const states = ['queued', 'on_machine', 'transmitting', 'acceptance_unconfirmed', 'accepted', 'rejected'] as const;
  api.histories.set(id, states.map((state, index) => inputItem(api, state, state, String(index + 1), state)));
  api.bindConversation(id);
  await page.reload();
  for (const text of ['В очереди', 'На машине', 'Передаётся', 'Приём не подтверждён', 'Принято агентом', 'Отклонено']) {
    await expect(page.getByText(text, { exact: false }).first()).toBeVisible();
  }
});

test('transmitting_bubble_waits_quietly_and_escalates_only_when_unconfirmed', async ({ page }) => {
  const { hub, api, id } = await boot(page, 0);
  const input = inputItem(api, 'quiet', 'transmitting', '1', 'тихо ждёт подтверждения');
  api.histories.set(id, [input]);
  const detail = api.details.get(id);
  if (detail) detail.execution = { state: 'running' };
  api.bindConversation(id);
  await page.reload();
  const bubble = page.locator('.bubble', { hasText: 'тихо ждёт подтверждения' });
  await expect(bubble).toBeVisible();
  await expect(bubble).toContainText('Передаётся');
  await expect(bubble.getByRole('button', { name: 'Отправить повторно как новое' })).toHaveCount(0);
  await expect(page.getByText('Приём сообщения не подтверждён')).toHaveCount(0);

  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: { input_id: input.id, state: 'acceptance_unconfirmed' }
  });
  hub.append(scope, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: {
      pause_reasons: [{ reason: 'unconfirmed_delivery', input_id: input.id }],
      hold_reasons: [],
      run_slot: 'free'
    }
  });
  hub.flush();
  await expect(bubble).toContainText('Приём не подтверждён');
  await expect(bubble.getByRole('button', { name: 'Отправить повторно как новое' })).toBeVisible();
  await expect(page.getByText('Приём сообщения не подтверждён')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Продолжить очередь' })).toBeVisible();

  hub.append(scope, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: { input_id: input.id, state: 'accepted' }
  });
  hub.append(scope, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: input.id } } }
  });
  hub.flush();
  await expect(page.getByText('Приём сообщения не подтверждён')).toHaveCount(0);
  await expect(bubble.getByRole('button', { name: 'Отправить повторно как новое' })).toHaveCount(0);
  await expect(bubble).toContainText(/Принято агентом|Агент работает/);
});

test('incomplete_block_is_marked_and_partial_text_stays', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const block = textBlock(api, 'частичный текст', '1', 'incomplete');
  block.incomplete_reason = 'process_exited';
  api.histories.set(id, [block]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.getByText('частичный текст')).toBeVisible();
  await expect(page.getByText('Неполный ответ')).toBeVisible();
  await expect(page.getByText('Процесс агента завершился')).toBeVisible();
});

test('interrupted_tool_reads_stopped_like_the_text_next_to_it', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  const call = toolBlock(api, '');
  call.tool_name = 'Bash';
  call.summary = 'Bash: sleep 90';
  call.state = 'incomplete';
  call.incomplete_reason = 'process_exited';
  call.created_seq = '1';
  const text = textBlock(api, 'частичный текст ответа', '2', 'incomplete');
  text.incomplete_reason = 'interrupted';
  api.histories.set(id, [call, text]);
  api.bindConversation(id);
  await page.reload();
  const row = page.locator('.tool').filter({ hasText: 'sleep 90' });
  await expect(row).toBeVisible({ timeout: 15_000 });
  // Одна остановка — одно слово и один цвет, а не «Ошибка» рядом с «Прервано».
  await expect(row.locator('.pill')).toHaveText('Прервано');
  await expect(row.locator('.pill')).toHaveAttribute('data-state', 'stopped');
  await expect(page.locator('[role="log"] .pill[data-state="error"]')).toHaveCount(0);
  const warning = await page.evaluate(() => { const probe = document.createElement('span'); probe.style.color = 'var(--warning)'; document.body.append(probe); const value = getComputedStyle(probe).color; probe.remove(); return value; });
  await expect(row.locator('.pill')).toHaveCSS('color', warning);
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

test('answer_card_footer_shows_agent_time_and_info_button_and_leaves_the_model_to_the_sheet', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  api.histories.set(id, [{ ...textBlock(api, 'ответ', '1'), agent: 'agent', input_command_id: api.id() }]);
  api.bindConversation(id);
  await page.reload();
  const footer = page.locator('footer[data-role="answer-footer"]');
  await expect(footer).toContainText('Agent');
  await expect(footer).not.toContainText('opus');
  await expect(footer.getByRole('button', { name: 'Сведения о ходе' })).toBeVisible();
});

test('answer_card_without_a_turn_has_no_info_button_but_keeps_the_footer_height', async ({ page }) => {
  const { api, id } = await boot(page, 0);
  api.histories.set(id, [{ ...textBlock(api, 'с ходом', '1'), input_command_id: api.id() }, textBlock(api, 'без хода', '2')]);
  api.bindConversation(id);
  await page.reload();
  const footers = page.locator('footer[data-role="answer-footer"]');
  await expect(footers).toHaveCount(2);
  await expect(footers.nth(0).getByRole('button', { name: 'Сведения о ходе' })).toBeVisible();
  await expect(footers.nth(1).getByRole('button', { name: 'Сведения о ходе' })).toHaveCount(0);
  const heights = await footers.evaluateAll((list) => list.map((node) => node.getBoundingClientRect().height));
  expect(Math.abs(heights[0]! - heights[1]!)).toBeLessThanOrEqual(0.5);
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

async function setMotion(page: import('@playwright/test').Page, motion: 'normal' | 'off'): Promise<void> {
  await page.addInitScript((value) => {
    try { localStorage.setItem('canterlot.motion', value); } catch { /* Настройка действует и без хранилища. */ }
  }, motion);
}

function liveText(count: number): string {
  return Array.from({ length: count }, (_, index) => `слово${index}`).join(' ');
}

test('streamed_answer_text_appears_progressively_and_finishes_exactly_as_received', async ({ page }) => {
  await setMotion(page, 'normal');
  const { hub, api, id } = await boot(page, 0);
  const blockId = api.id();
  const scope = `conversation:${id}`;
  const text = liveText(28);
  hub.append(scope, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: 0, rev: 1, append: text } });
  hub.flush();

  const block = page.locator(`[data-block="${blockId}"]`);
  await expect(block).toBeVisible();
  const samples: string[] = [];
  for (let step = 0; step < 8; step++) {
    samples.push(((await block.textContent()) ?? '').trim());
    await page.waitForTimeout(40);
  }
  // Текст не вываливается целиком: в выборке есть непустой префикс, но не весь ответ.
  expect(samples.some((sample) => sample.length > 0 && sample !== text)).toBe(true);
  expect(samples.every((sample) => text.startsWith(sample))).toBe(true);

  hub.append(scope, {
    kind: 'block_final', conversation_id: id,
    payload: { block_id: blockId, rev: 2, text, checksum: await sha256Hex(text), text_omitted: false, summary: null }
  });
  hub.flush();
  await expect(block).toHaveText(text);
  expect(((await block.textContent()) ?? '').trim()).toBe(text);
  await expect(block.locator('.reveal-word')).toHaveCount(0);
});

test('motion_off_shows_streamed_text_at_once', async ({ page }) => {
  await setMotion(page, 'off');
  const { hub, api, id } = await boot(page, 0);
  const blockId = api.id();
  const text = liveText(28);
  hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.append(`conversation:${id}`, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: 0, rev: 1, append: text } });
  hub.flush();
  const block = page.locator(`[data-block="${blockId}"]`);
  // Обычная скорость проявила бы 28 слов почти за секунду; «Выкл» — сразу.
  await expect(block).toHaveText(text, { timeout: 700 });
  await expect(block.locator('.reveal-word')).toHaveCount(0);
});

test('history_after_reload_shows_answer_text_at_once', async ({ page }) => {
  await setMotion(page, 'normal');
  const { api, id } = await boot(page, 0);
  const text = liveText(40);
  const block = textBlock(api, text, '1');
  api.histories.set(id, [block]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible();
  const live = page.locator(`[data-block="${block.id}"]`);
  await expect(live).toBeVisible();
  expect(((await live.textContent()) ?? '').trim()).toBe(text);
  await expect(live.locator('.reveal-word')).toHaveCount(0);
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
  const before = await settled(() => feed.evaluate((el) => el.scrollTop), (a, b) => Math.abs(a - b) <= 0.5);
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
  await expect(shot).toContainText('Картинка загружается');
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

const PNG_2X2 = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGPQmNbz//9/BggFADYPCI8NpaO2AAAAAElFTkSuQmCC', 'base64');

function withFiles(item: HistoryInput): HistoryInput {
  item.attachments = [
    { attachment_id: 'att-img', name: 'Screenshot.png', mime: 'image/png', size: 2048, sha256: '', image: true },
    { attachment_id: 'att-big', name: 'huge.png', mime: 'image/png', size: 60 * 1024 * 1024, sha256: '', image: true },
    { attachment_id: 'att-vid', name: 'screen-record.mp4', mime: 'video/mp4', size: 7 * 1024 * 1024, sha256: '', image: false },
    { attachment_id: 'att-doc', name: 'report.pdf', mime: 'application/pdf', size: 1536, sha256: '', image: false }
  ];
  return item;
}

for (const theme of ['dark', 'light'] as const) {
  test(`own_attachments_show_thumbnail_tiles_and_open_in_the_viewer_${theme}`, async ({ page }, info) => {
    const fetched: string[] = [];
    const { api, id } = await boot(page, 0);
    await page.route('**/api/v1/attachments/*/content', async (route) => {
      fetched.push(route.request().url());
      await route.fulfill({ contentType: 'image/png', body: PNG_2X2 });
    });
    api.histories.set(id, [withFiles(inputItem(api, 'files', 'accepted', '1', 'с вложениями'))]);
    api.bindConversation(id);
    await page.evaluate((value) => localStorage.setItem('canterlot.theme', value), theme);
    await page.reload();
    const bubble = page.locator('.bubble', { hasText: 'с вложениями' });
    await expect(bubble).toBeVisible();
    const thumb = bubble.locator('img');
    await expect(thumb).toHaveCount(1);
    await expect(thumb).toHaveAttribute('src', '/api/v1/attachments/att-img/content');
    await expect(thumb).toHaveAttribute('loading', 'lazy');
    await expect.poll(() => thumb.evaluate((el: HTMLImageElement) => el.complete && el.naturalWidth)).toBe(2);
    // Огромный оригинал, видео и документ не тянутся для миниатюры: байт вложений они не запрашивают.
    expect(fetched.every((url) => url.includes('att-img'))).toBe(true);
    await expect(bubble).toContainText('huge.png');
    await expect(bubble).toContainText('60 МиБ');
    await expect(bubble.locator('a[data-kind="video"]')).toContainText('screen-record.mp4');
    await expect(bubble.locator('a[data-kind="video"]')).toContainText('7 МиБ');
    await expect(bubble.getByRole('link', { name: /report\.pdf/ })).toContainText('1,5 КиБ');
    const box = await bubble.boundingBox();
    const viewport = page.viewportSize()!;
    expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width);
    await page.screenshot({ path: `/tmp/shots-t10/attachments-${theme}-${info.project.name}.png` });
    await bubble.getByRole('link', { name: 'Открыть картинку Screenshot.png' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog.locator('.slide[data-current] img.full')).toHaveAttribute('src', '/api/v1/attachments/att-img/content');
    await expect(dialog).toContainText('Screenshot.png');
    await page.screenshot({ path: `/tmp/shots-t10/attachments-viewer-${theme}-${info.project.name}.png` });
  });
}

test('empty_chat_says_so_and_a_loading_chat_shows_placeholders', async ({ page }) => {
  // Пока снимок в пути, вместо пустоты три полосы заготовки, как в шапке.
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  let release: () => void = () => undefined;
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route('**/snapshot', async (route) => { await held; await route.fallback(); });
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-feed-loading] .skeleton')).toHaveCount(3);
  release();
  await expect(page.locator('[data-feed-loading]')).toHaveCount(0);
  await expect(page.locator('[data-feed-empty]')).toHaveText('Сообщений пока нет. Напишите агенту задачу');
});

test('a_chat_with_history_never_flashes_the_empty_line', async ({ page }) => {
  await page.addInitScript(() => {
    (window as unknown as { __empty: number }).__empty = 0;
    new MutationObserver(() => { if (document.querySelector('[data-feed-empty]')) (window as unknown as { __empty: number }).__empty += 1; }).observe(document, { childList: true, subtree: true });
  });
  await boot(page, 5);
  expect(await page.evaluate(() => (window as unknown as { __empty: number }).__empty)).toBe(0);
});
