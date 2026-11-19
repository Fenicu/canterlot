import type { Page } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedSchedule } from '../../testing/fixtures/schedules';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SCHEDULE = '00000000-0000-4000-8000-0000000000b1';
const REPORT = '00000000-0000-4000-8000-0000000000c1';
const BASE = Date.parse('2026-11-18T08:00:00.000Z');

const BODY = [
  '## Вчера сделано',
  '- Починил разбор расписаний',
  '- Дописал тесты карточки',
  '',
  '## Сегодня в планах',
  '1. Выкатить рутины',
  '',
  '```ts',
  'const done = true;',
  '```'
].join('\n');

function block(api: FakeHubApi, seq: number, text: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text, rev: 1, state: 'final' });
  return {
    kind: 'block', id, block_kind: 'text', content: text, content_bytes: text.length, content_truncated: false,
    created_seq: String(seq), created_at: new Date(BASE + seq * 60_000).toISOString(), rev: 1, state: 'final',
    agent: 'agent', model: 'opus', tool_name: null, summary: null, parent_block_id: null, input_command_id: null,
    line_count: text.split('\n').length
  };
}

async function boot(page: Page, opts: { history?: number; path?: (id: string) => string; seed?: (api: FakeHubApi, id: string) => void; width?: number } = {}) {
  if (opts.width) await page.setViewportSize({ width: opts.width, height: 800 });
  await page.addInitScript(() => {
    const copied: string[] = [];
    (window as unknown as { __copied: string[] }).__copied = copied;
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async (text: string) => { copied.push(text); } } });
  });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Работа' });
  const count = opts.history ?? 6;
  api.histories.set(id, Array.from({ length: count }, (_, index) => block(api, index + 1, `Ответ ${index + 1}`)));
  api.bindConversation(id);
  seedSchedule(api, { id: SCHEDULE, name: 'Стендап', target: { kind: 'session_report', conversation_id: id } });
  opts.seed?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(opts.path ? opts.path(id) : `/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

const card = (page: Page) => page.locator('[data-role="routine-report"]');
const AFTER_ALL = new Date(BASE + 3_600_000).toISOString();

test('pending_card_turns_ready_through_events_and_renders_markdown', async ({ page }) => {
  const { api, id } = await boot(page, {
    seed: (hub, conversation) => { hub.putRoutineReport(conversation, { id: REPORT, schedule_id: SCHEDULE, created_at: AFTER_ALL }); }
  });
  await expect(card(page)).toHaveCount(1);
  await expect(card(page)).toContainText('Рутина · Утренний стендап');
  await expect(card(page)).toHaveAttribute('data-state', 'pending');
  await expect(card(page)).toContainText('Рутина собирает отчёт…');
  await expect(card(page).getByRole('button', { name: 'Завершить' })).toHaveCount(0);

  api.putRoutineReport(id, { id: REPORT, state: 'ready', body: BODY, completed_at: AFTER_ALL });
  await expect(card(page)).toHaveAttribute('data-state', 'ready');
  await expect(card(page).getByRole('heading', { name: 'Вчера сделано' })).toBeVisible();
  await expect(card(page).locator('li')).toHaveCount(3);
  await expect(card(page).locator('pre code')).toContainText('const done = true;');
  await expect(card(page).getByRole('button', { name: 'Завершить' })).toBeVisible();
  await expect(card(page).getByRole('button', { name: 'Скопировать', exact: true })).toBeVisible();
  await expect(card(page)).not.toContainText('Рутина собирает отчёт…');
});

test('the_card_is_the_owners_only_and_leaves_the_session_alone', async ({ page }) => {
  await boot(page, {
    seed: (api, id) => { api.putRoutineReport(id, { id: REPORT, schedule_id: SCHEDULE, state: 'ready', body: BODY, created_at: AFTER_ALL }); }
  });
  await expect(card(page)).toBeVisible();
  await expect(card(page).locator('[data-role="bookmark"], [data-role="answer-info"], [aria-label*="закладк" i]')).toHaveCount(0);
  await expect(card(page).getByRole('button', { name: /Об ответе/ })).toHaveCount(0);
  await expect(page.locator('[data-row][data-kind="answer"]')).toHaveCount(6);
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeEnabled();
  await expect(page.locator('[data-role="pending-strip"], [data-pending-strip]')).toHaveCount(0);
});

test('dismiss_removes_the_card_and_another_clients_card_goes_through_the_event', async ({ page }) => {
  const other = '00000000-0000-4000-8000-0000000000c2';
  const { api, id } = await boot(page, {
    seed: (hub, conversation) => {
      hub.putRoutineReport(conversation, { id: REPORT, schedule_id: SCHEDULE, state: 'ready', body: BODY, created_at: AFTER_ALL });
      hub.putRoutineReport(conversation, { id: other, schedule_id: '00000000-0000-4000-8000-0000000000b2', title: 'Недельный план', state: 'ready', body: 'План', created_at: new Date(BASE + 4_000_000).toISOString() });
    }
  });
  await expect(card(page)).toHaveCount(2);
  await card(page).filter({ hasText: 'Утренний стендап' }).getByRole('button', { name: 'Завершить' }).click();
  await expect(card(page)).toHaveCount(1);
  expect(api.routineDismissals).toEqual([{ conversation: id, report: REPORT }]);

  // Закрыли на другом устройстве: хаб рассылает событие, карточка уходит и здесь.
  api.dismissRoutineReport(id, other);
  await expect(card(page)).toHaveCount(0);
});

test('a_replaced_card_disappears_and_the_new_one_takes_its_place', async ({ page }) => {
  const next = '00000000-0000-4000-8000-0000000000c3';
  const { api, id } = await boot(page, {
    seed: (hub, conversation) => { hub.putRoutineReport(conversation, { id: REPORT, schedule_id: SCHEDULE, state: 'ready', body: 'Старый отчёт', created_at: AFTER_ALL }); }
  });
  await expect(card(page)).toContainText('Старый отчёт');
  api.dismissRoutineReport(id, REPORT, 'replaced');
  api.putRoutineReport(id, { id: next, schedule_id: SCHEDULE, state: 'ready', body: 'Новый отчёт', created_at: new Date(BASE + 4_000_000).toISOString() });
  await expect(card(page)).toHaveCount(1);
  await expect(card(page)).toContainText('Новый отчёт');
  await expect(card(page)).not.toContainText('Старый отчёт');
});

test('copy_puts_the_report_text_on_the_clipboard_with_the_usual_feedback', async ({ page }) => {
  await boot(page, {
    seed: (api, id) => { api.putRoutineReport(id, { id: REPORT, schedule_id: SCHEDULE, state: 'ready', body: BODY, created_at: AFTER_ALL }); }
  });
  await card(page).getByRole('button', { name: 'Скопировать', exact: true }).click();
  await expect(card(page).getByRole('status')).toContainText('Скопировано');
  expect(await page.evaluate(() => (window as unknown as { __copied: string[] }).__copied)).toEqual([BODY]);
});

test('failed_card_explains_the_reason_and_retry_runs_the_schedule_now', async ({ page }) => {
  const { api, id } = await boot(page, {
    seed: (hub, conversation) => {
      hub.putRoutineReport(conversation, {
        id: REPORT, schedule_id: SCHEDULE, state: 'failed', created_at: AFTER_ALL,
        error: { reason: 'turn_timeout', message: 'Копия не закончила отчёт за 60 минут' }
      });
    }
  });
  await expect(card(page)).toHaveAttribute('data-state', 'failed');
  await expect(card(page)).toContainText('Копия сессии не ответила за 60 минут');
  await expect(card(page).getByRole('button', { name: 'Скопировать', exact: true })).toHaveCount(0);
  await card(page).getByRole('button', { name: 'Повторить' }).click();
  await expect.poll(() => api.scheduleRuns.get(SCHEDULE)?.length ?? 0).toBe(1);
  expect(api.scheduleRuns.get(SCHEDULE)![0]).toMatchObject({ manual: true });
  await card(page).getByRole('button', { name: 'Завершить' }).click();
  await expect(card(page)).toHaveCount(0);
  expect(api.routineDismissals).toEqual([{ conversation: id, report: REPORT }]);
});

test('deep_link_scrolls_to_the_card_and_highlights_it_briefly', async ({ page }) => {
  await boot(page, {
    history: 160,
    // Время карточки между ответами: ей место в середине длинной ленты, а не у хвоста.
    seed: (api, id) => { api.putRoutineReport(id, { id: REPORT, schedule_id: SCHEDULE, state: 'ready', body: BODY, created_at: new Date(BASE + 40.5 * 60_000).toISOString() }); },
    path: (id) => `/c/${id}?routine_report=${REPORT}`
  });
  await expect(card(page)).toBeInViewport({ timeout: 10_000 });
  await expect(page.locator('[data-row][data-jump]')).toHaveCount(1);
  await expect(page.locator('[data-row][data-jump]')).toContainText('Рутина · Утренний стендап');
  await expect(page.locator('[data-row][data-jump]')).toHaveCount(0, { timeout: 5_000 });
});

test('deep_link_to_a_missing_card_just_opens_the_chat', async ({ page }) => {
  await boot(page, { path: (id) => `/c/${id}?routine_report=${REPORT}` });
  await expect(card(page)).toHaveCount(0);
  await expect(page.locator('[data-row][data-jump]')).toHaveCount(0);
  await expect(page.getByText('Ответ 6')).toBeVisible();
});

for (const width of [360, 390, 412]) {
  test(`card_fits_the_phone_width_${width}`, async ({ page }) => {
    const long = 'https://example.org/' + 'очень-длинная-ссылка-'.repeat(8);
    const body = `${BODY}\n\n${long}\n\n\`\`\`\n${'x'.repeat(200)}\n\`\`\``;
    await boot(page, {
      width,
      seed: (api, id) => {
        api.putRoutineReport(id, { id: REPORT, schedule_id: SCHEDULE, state: 'ready', title: 'Очень длинное название рутины для проверки переносов', body, created_at: AFTER_ALL });
      }
    });
    await expect(card(page)).toBeVisible();
    const geometry = await page.evaluate(() => {
      const root = document.querySelector('[data-role="routine-report"]') as HTMLElement;
      const box = root.getBoundingClientRect();
      const buttons = [...root.querySelectorAll('button')].filter((item) => item.textContent?.trim()).map((item) => item.getBoundingClientRect());
      const feed = document.querySelector('.feed') as HTMLElement;
      return {
        left: box.left, right: box.right, viewport: window.innerWidth,
        docOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        feedOverflow: feed.scrollWidth - feed.clientWidth,
        buttons: buttons.map((item) => ({ left: item.left, right: item.right, top: item.top, bottom: item.bottom }))
      };
    });
    expect(geometry.left).toBeGreaterThanOrEqual(0);
    expect(geometry.right).toBeLessThanOrEqual(geometry.viewport);
    expect(geometry.docOverflow).toBeLessThanOrEqual(0);
    expect(geometry.feedOverflow).toBeLessThanOrEqual(0);
    expect(geometry.buttons.length).toBeGreaterThanOrEqual(2);
    for (let a = 0; a < geometry.buttons.length; a++) {
      const one = geometry.buttons[a]!;
      expect(one.left).toBeGreaterThanOrEqual(geometry.left - 1);
      expect(one.right).toBeLessThanOrEqual(geometry.right + 1);
      for (let b = a + 1; b < geometry.buttons.length; b++) {
        const two = geometry.buttons[b]!;
        const apart = one.right <= two.left + 0.5 || two.right <= one.left + 0.5 || one.bottom <= two.top + 0.5 || two.bottom <= one.top + 0.5;
        expect(apart).toBe(true);
      }
    }
  });
}
