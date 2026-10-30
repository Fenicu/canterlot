import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, seed?: (api: FakeHubApi, id: string) => void) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Восстановление потока после сна', model: 'opus-5.5' });
  seed?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function block(api: FakeHubApi, seq: string, partial: Partial<HistoryBlock>): HistoryBlock {
  const id = api.id();
  const content = partial.content ?? 'вывод';
  const at = new Date(Date.now() - 60_000).toISOString();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return { kind: 'block', id, block_kind: 'text', content, content_bytes: content.length, content_truncated: false, created_seq: seq, created_at: at, rev: 1, state: 'final', agent: 'claude', model: 'opus-5.5', tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1, started_at: at, finished_at: new Date(Date.now() - 59_000).toISOString(), ...partial };
}

function startTurn(hub: FakeHub, api: FakeHubApi, id: string): void {
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'derived_changed', conversation_id: id, payload: { conversation_id: id, activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() }, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: new Date().toISOString(), settings: { model: 'opus-5.5', effort: 'high', permission_mode: 'default' }, crunch: { enabled: false } } });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: api.id() } } } });
  hub.flush();
}

test.describe('поле ввода', () => {
  test('resting_composer_shows_no_error_and_keeps_the_reason_for_screen_readers', async ({ page }) => {
    await boot(page);
    const field = page.getByRole('textbox', { name: 'Сообщение' });
    await expect(page.locator('[data-composer] .reason')).toHaveCount(0);
    await expect(field).toHaveAttribute('placeholder', 'Сообщение агенту…');
    await expect(field).toHaveAccessibleDescription('Введите сообщение или прикрепите файл');
  });

  test('send_stays_on_one_row_with_attach_when_interrupt_appears', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    await page.getByRole('textbox', { name: 'Сообщение' }).fill('уточнение');
    const send = page.getByRole('button', { name: 'Отправить', exact: true });
    const before = await send.boundingBox();
    startTurn(hub, api, id);
    await expect(page.getByRole('button', { name: 'Прервать' })).toBeVisible();
    const after = await send.boundingBox();
    const attach = await page.getByRole('button', { name: 'Прикрепить файл' }).boundingBox();
    expect(after!.x).toBeCloseTo(before!.x, 0);
    expect(after!.y).toBeCloseTo(before!.y, 0);
    expect(attach!.y).toBeCloseTo(after!.y, 0);
  });

  test('field_grows_up_to_six_lines_and_the_capsule_carries_the_focus_ring', async ({ page }) => {
    await boot(page);
    const field = page.getByRole('textbox', { name: 'Сообщение' });
    const empty = (await field.boundingBox())!.height;
    await field.fill(['раз', 'два', 'три'].join('\n'));
    await expect.poll(async () => (await field.boundingBox())!.height).toBeGreaterThan(empty * 2);
    await field.fill(Array.from({ length: 14 }, (_, index) => `строка ${index}`).join('\n'));
    await expect.poll(async () => (await field.boundingBox())!.height).toBeGreaterThan(empty * 5);
    const capped = (await field.boundingBox())!.height;
    expect(capped).toBeLessThanOrEqual(empty * 6 + 1);
    await field.focus();
    const ring = await page.locator('.capsule').evaluate((node) => getComputedStyle(node).outlineStyle);
    expect(ring).toBe('solid');
    expect(await field.evaluate((node) => getComputedStyle(node).outlineStyle)).toBe('none');
  });
});

test.describe('строки инструментов и сессий', () => {
  test('tool_row_keeps_the_name_whole_and_the_status_at_the_right_edge', async ({ page }) => {
    const { api, id } = await boot(page);
    api.histories.set(id, [block(api, '1', { block_kind: 'tool_call', tool_name: 'Bash', summary: 'cargo test -p canterlot-hub --test stream -- --nocapture reconnect_after_sleep', content: 'ok' })]);
    api.bindConversation(id);
    await page.reload();
    const head = page.locator('.tool .head');
    await expect(head).toBeVisible();
    const box = await head.evaluate((node) => {
      const name = node.querySelector('.name')!;
      const summary = node.querySelector('.summary')!;
      const pill = node.querySelector('.pill')!;
      const style = getComputedStyle(name);
      return { nameHeight: name.getBoundingClientRect().height, line: parseFloat(style.lineHeight) || parseFloat(style.fontSize) * 1.6, summaryLines: summary.getBoundingClientRect().height, gap: node.getBoundingClientRect().right - pill.getBoundingClientRect().right, ellipsis: getComputedStyle(summary).textOverflow, chevron: node.querySelectorAll('svg').length };
    });
    expect(box.nameHeight).toBeLessThan(box.line * 1.5);
    expect(box.summaryLines).toBeLessThan(box.line * 1.5);
    expect(box.gap).toBeLessThan(24);
    expect(box.ellipsis).toBe('ellipsis');
    expect(box.chevron).toBe(1);
  });

  test('session_row_shows_unread_or_time_and_never_lays_them_over_the_meta_line', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedMockupSessions(api);
    for (const row of api.list.values()) row.last_activity_at = new Date().toISOString();
    await installChatRoutes(page, hub, api, ids.pinned);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    const rows = await page.locator('[data-session]').evaluateAll((nodes) => nodes.map((node) => {
      const time = node.querySelector('time')?.getBoundingClientRect();
      const meta = node.querySelector('.meta')!.getBoundingClientRect();
      const title = node.querySelector('.title')!.getBoundingClientRect();
      return { time: time ? { top: time.top, bottom: time.bottom, left: time.left } : null, metaTop: meta.top, titleRight: title.right, unread: node.querySelector('.unread, .count') !== null };
    }));
    expect(rows.length).toBeGreaterThan(3);
    for (const row of rows) {
      if (row.unread) expect(row.time).toBeNull();
      if (row.time) {
        expect(row.time.bottom).toBeLessThanOrEqual(row.metaTop + 1);
        expect(row.time.left).toBeGreaterThanOrEqual(row.titleRight - 1);
      }
    }
  });
});

test.describe('шапка и сообщения', () => {
  test('phone_header_has_one_back_control_and_a_full_width_title_row', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, id } = await boot(page);
    await expect(page.getByRole('link', { name: 'Назад' })).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Все сессии' })).toHaveCount(1);
    hub.append(`conversation:${id}`, { kind: 'derived_changed', conversation_id: id, payload: { conversation_id: id, activity: { state: 'waiting_for_user', evidence: { question: 'Что делать, если курсор старше журнала событий и повтор уже невозможен?' }, since: new Date().toISOString() }, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: new Date().toISOString(), settings: { model: 'opus-5.5', effort: 'high', permission_mode: 'default' }, crunch: { enabled: false } } });
    hub.flush();
    await expect(page.locator('.state-chip .state-text')).toHaveText('Ждёт ответа');
    const sizes = await page.locator('.chat-header').evaluate((node) => ({
      title: node.querySelector('.header-title')!.getBoundingClientRect().width,
      chip: node.querySelector('.state-chip')!.getBoundingClientRect().height,
      bar: node.querySelector('.topbar')!.getBoundingClientRect().height
    }));
    expect(sizes.title).toBeGreaterThan(190);
    expect(sizes.chip).toBeLessThan(24);
    expect(sizes.bar).toBeLessThan(70);
    await expect(page.locator('.chat-header')).not.toContainText('курсор старше');
  });

  test('bubbles_keep_a_gutter_sit_right_and_only_the_last_input_shows_the_working_track', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    const input = (text: string, seq: string) => ({ kind: 'input', id: api.id(), idempotency_key: `k-${seq}`, state: 'accepted', content: [{ type: 'text', text }], attachments: [], created_seq: seq, feed_seq: seq, created_at: new Date().toISOString() });
    api.histories.set(id, [input('Первое', '1'), block(api, '2', { content: 'Ответ' }), input('Второе', '3'), block(api, '4', { content: 'Идёт', state: 'streaming' })] as never);
    api.bindConversation(id);
    await page.reload();
    await expect(page.locator('article.bubble')).toHaveCount(2);
    const boxes = await page.locator('article.bubble').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect()).map(({ x, right }) => ({ x, right })));
    for (const box of boxes) {
      expect(box.x).toBeGreaterThan(8);
      expect(box.right).toBeLessThanOrEqual(390 - 8);
    }
    await expect(page.locator('article.bubble').first()).toContainText('Принято агентом');
    await expect(page.locator('article.bubble').first()).not.toContainText('Агент работает');
    await expect(page.locator('article.bubble').nth(1).locator('.st-status')).toHaveText('Агент работает');
    await expect(page.locator('[data-row]').nth(2)).toBeVisible();
    await expect(page.getByText('Принято агентом', { exact: false })).toHaveCount(1);
    void hub;
  });

  test('new_messages_pill_sits_at_the_bottom_centre_of_the_feed', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    api.histories.set(id, Array.from({ length: 30 }, (_, index) => block(api, String(index - 30), { content: `Ответ ${index}\n\nЕщё абзац текста, чтобы лента прокручивалась.` })));
    api.bindConversation(id);
    await page.reload();
    await expect(page.locator('[data-row]').first()).toBeVisible();
    const feed = page.locator('.feed');
    await expect.poll(() => feed.evaluate((node) => node.scrollHeight - node.clientHeight)).toBeGreaterThan(500);
    await feed.evaluate((node) => { node.scrollTop -= 500; });
    await page.waitForTimeout(200);
    hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: { block_id: api.id(), rev: 0, kind: 'text', input_command_id: null, parent_block_id: null, agent: 'claude', model: 'opus-5.5', tool_name: null, summary: null, approved_by: null } });
    hub.flush();
    const pill = page.getByRole('button', { name: 'К новым сообщениям' });
    await expect(pill).toBeVisible();
    const box = (await pill.boundingBox())!;
    const wrap = (await page.locator('.feed-wrap').boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(44);
    expect(Math.abs(box.x + box.width / 2 - (wrap.x + wrap.width / 2))).toBeLessThan(2);
    expect(wrap.y + wrap.height - (box.y + box.height)).toBeLessThan(24);
  });
});
