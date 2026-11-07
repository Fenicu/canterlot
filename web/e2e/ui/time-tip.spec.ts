import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

test.use({ timezoneId: 'Europe/Moscow' });

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const START = Date.parse('2026-11-06T13:00:00.000Z');
const FIRST = 'пятница, 6 ноября 2026, 16:01:00';
const LAST = 'пятница, 6 ноября 2026, 16:19:00';
const OWNER_LAST = 'пятница, 6 ноября 2026, 16:18:00';
const TIP = '.time-tip[role="tooltip"]';

function block(api: FakeHubApi, seq: number, at: number, run: string): HistoryBlock {
  const id = api.id();
  const content = `ответ ${run.slice(-2)}`;
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content,
    content_bytes: content.length,
    content_truncated: false,
    created_seq: String(seq),
    created_at: new Date(at).toISOString(),
    rev: 1,
    state: 'final',
    agent: 'claude',
    model: 'opus-5.5',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: run,
    line_count: 1,
    started_at: new Date(at).toISOString(),
    finished_at: new Date(at + 1000).toISOString()
  };
}

function input(api: FakeHubApi, seq: number, at: number): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `idem-${seq}`,
    state: 'accepted',
    content: [{ type: 'text', text: `сообщение ${seq}` }],
    attachments: [],
    created_seq: String(seq),
    feed_seq: String(seq),
    created_at: new Date(at).toISOString()
  };
}

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Время сообщений' });
  const items: (HistoryBlock | HistoryInput)[] = [];
  for (let i = 0; i < 10; i++) {
    const at = START + i * 120_000;
    const run = `run-${i}`;
    items.push(input(api, i * 2 + 1, at), block(api, i * 2 + 2, at + 60_000, run));
  }
  api.histories.set(id, items);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('[data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

const answerTimes = (page: Page) => page.locator('[data-role="answer-footer"] time');

/** Контраст текста подсказки с её собственным фоном по WCAG. */
function textContrast(node: Locator): Promise<number> {
  return node.evaluate((element) => {
    const rgb = (value: string) => (value.match(/[\d.]+/g) ?? []).map(Number);
    const luminance = (parts: number[]) => parts.map((part) => {
      const channel = part / 255;
      return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
    }).reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index]!, 0);
    const style = getComputedStyle(element);
    const [light, dark] = [luminance(rgb(style.color)), luminance(rgb(style.backgroundColor))].sort((a, b) => b - a);
    return (light! + 0.05) / (dark! + 0.05);
  });
}
const tip = (page: Page) => page.locator(TIP);

/** Подсказка доиграла вход: до этого её opacity меньше единицы и она просвечивает. */
async function settle(node: Locator): Promise<void> {
  await expect.poll(() => node.evaluate((element) => element.getAnimations().length)).toBe(0);
  await expect(node).toHaveCSS('opacity', '1');
}

function backgroundAlpha(node: Locator): Promise<number> {
  return node.evaluate((element) => {
    const channels = (getComputedStyle(element).backgroundColor.match(/[\d.]+/g) ?? []).map(Number);
    return channels.length === 4 ? channels[3]! : 1;
  });
}

function outline(node: Locator): Promise<{ style: string; width: number; alpha: number }> {
  return node.evaluate((element) => {
    const style = getComputedStyle(element);
    const channels = (style.borderTopColor.match(/[\d.]+/g) ?? []).map(Number);
    return {
      style: style.borderTopStyle,
      width: Number.parseFloat(style.borderTopWidth),
      alpha: channels.length === 4 ? channels[3]! : 1
    };
  });
}

async function reveal(page: Page, time = answerTimes(page).last()) {
  await time.scrollIntoViewIfNeeded();
  if (test.info().project.name === 'phone') await time.tap();
  else await time.hover();
  await expect(tip(page)).toBeVisible();
  return time;
}

test('hover_shows_the_full_date_after_the_delay_and_leave_hides_it', async ({ page }) => {
  test.skip(test.info().project.name === 'phone', 'наведение проверяется мышью');
  await boot(page);
  const time = answerTimes(page).last();
  await time.scrollIntoViewIfNeeded();
  // Задержку меряет сама страница: от входа указателя до появления подсказки. Таймер теста под нагрузкой опаздывает сам.
  await time.evaluate((node) => {
    const marks = ((window as unknown as { __tip: { enter?: number; shown?: number } }).__tip = {});
    node.addEventListener('pointerenter', () => { marks.enter ??= performance.now(); }, { once: true });
    new MutationObserver((_, observer) => {
      if (document.querySelector('.time-tip[role="tooltip"]')) { marks.shown ??= performance.now(); observer.disconnect(); }
    }).observe(document.body, { childList: true, subtree: true });
  });
  await time.hover();
  await expect(tip(page)).toBeVisible();
  const marks = await page.evaluate(() => (window as unknown as { __tip: { enter?: number; shown?: number } }).__tip);
  expect(marks.enter).toBeDefined();
  expect(marks.shown! - marks.enter!).toBeGreaterThanOrEqual(250);
  await expect(tip(page)).toHaveText(LAST);
  await settle(tip(page));
  expect(await backgroundAlpha(tip(page))).toBe(1);
  const border = await outline(tip(page));
  expect(border.style).toBe('solid');
  expect(border.width).toBeGreaterThan(0);
  expect(border.alpha).toBe(1);
  expect(await textContrast(tip(page))).toBeGreaterThanOrEqual(4.5);
  await page.mouse.move(2, 2);
  await expect(tip(page)).toHaveCount(0);
});

test('the_tip_is_opaque_and_outlined_in_the_light_palette', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'light'));
  await boot(page);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await reveal(page);
  await settle(tip(page));
  expect(await backgroundAlpha(tip(page))).toBe(1);
  const border = await outline(tip(page));
  expect(border.style).toBe('solid');
  expect(border.width).toBeGreaterThan(0);
  expect(border.alpha).toBe(1);
  expect(await textContrast(tip(page))).toBeGreaterThanOrEqual(4.5);
});

test('keyboard_focus_shows_the_date_and_escape_hides_it', async ({ page }) => {
  await boot(page);
  const time = answerTimes(page).first();
  await time.scrollIntoViewIfNeeded();
  await page.waitForTimeout(150);
  await expect(time).toHaveAttribute('tabindex', '0');
  await time.focus();
  await expect(tip(page)).toBeVisible();
  await expect(tip(page)).toHaveText(FIRST);
  await expect(tip(page)).toHaveAttribute('role', 'tooltip');
  await expect(time).toHaveAttribute('aria-describedby', /^time-tip-\d+$/);
  await page.keyboard.press('Escape');
  await expect(tip(page)).toHaveCount(0);
  await expect(time).not.toHaveAttribute('aria-describedby', /time-tip-/);
  await expect(time).toBeFocused();
});

test('tap_toggles_the_tip_without_the_bubble_action_and_auto_hides', async ({ page }) => {
  test.skip(test.info().project.name !== 'phone', 'нажатие проверяется на телефоне');
  await boot(page);
  const time = page.locator('[data-row][data-kind="input"] time').last();
  await time.scrollIntoViewIfNeeded();
  const url = page.url();

  await time.tap();
  await expect(tip(page)).toBeVisible();
  await expect(tip(page)).toHaveText(OWNER_LAST);
  expect(page.url()).toBe(url);
  await expect(page.locator('dialog[open]')).toHaveCount(0);

  await time.tap();
  await expect(tip(page)).toHaveCount(0);

  await time.tap();
  await expect(tip(page)).toBeVisible();
  await page.waitForTimeout(3300);
  await expect(tip(page)).toHaveCount(0);
});

test('a_tap_elsewhere_closes_the_tip', async ({ page }) => {
  test.skip(test.info().project.name !== 'phone', 'нажатие проверяется на телефоне');
  await boot(page);
  const time = page.locator('[data-row][data-kind="input"] time').last();
  await time.scrollIntoViewIfNeeded();
  await time.tap();
  await expect(tip(page)).toBeVisible();
  await page.locator('.feed').tap({ position: { x: 12, y: 12 } });
  await expect(tip(page)).toHaveCount(0);
});

test('only_one_tooltip_is_open_at_a_time', async ({ page }) => {
  await boot(page);
  const times = answerTimes(page);
  const count = await times.count();
  await reveal(page, times.nth(count - 2));
  await reveal(page, times.nth(count - 1));
  await expect(tip(page)).toHaveCount(1);
  await expect(tip(page)).toHaveText(LAST);
});

test('the_tip_flips_below_at_the_top_of_the_feed', async ({ page }) => {
  await boot(page);
  const feed = page.locator('.feed');
  await feed.evaluate((node) => { node.scrollTop = 0; });
  await page.waitForTimeout(250);
  // Лента виртуализирует строки: двигаем саму прокрутку, а отметку ставим к её верхнему краю.
  await answerTimes(page).first().evaluate((node) => {
    const scroller = node.closest('.feed') as HTMLElement;
    scroller.scrollTop += node.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
  });
  await page.waitForTimeout(250);
  const time = answerTimes(page).first();
  await reveal(page, time);
  await expect(tip(page)).toHaveAttribute('data-placement', 'below');
  const [box, anchor] = await Promise.all([tip(page).boundingBox(), time.boundingBox()]);
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.y).toBeGreaterThanOrEqual(anchor!.y + anchor!.height);
});

test('reduced_motion_has_no_tip_animation', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await boot(page);
  await reveal(page);
  expect(await tip(page).evaluate((node) => getComputedStyle(node).animationName)).toBe('none');
});

test('a_session_row_shows_the_time_on_keyboard_focus_without_extra_tab_stops', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const row = page.locator('[data-session] a', { has: page.locator('time') }).first();
  const time = row.locator('time').first();
  await expect(time).not.toHaveAttribute('tabindex');
  await row.focus();
  await expect(tip(page)).toBeVisible();
  await expect(tip(page)).toHaveText(/^(понедельник|вторник|среда|четверг|пятница|суббота|воскресенье), \d+ [а-я]+ \d{4}, \d{2}:\d{2}:\d{2}$/);
});
