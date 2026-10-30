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
