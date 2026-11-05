import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { PALETTES } from '../../src/lib/palettes';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { bootSecondary } from './secondary-fixture';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

/** Считает вызовы startViewTransition, не меняя их поведения. */
async function countTransitions(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const target = window as unknown as { __transitions: number };
    target.__transitions = 0;
    const original = document.startViewTransition?.bind(document);
    if (!original) return;
    document.startViewTransition = ((update: ViewTransitionUpdateCallback) => { target.__transitions++; return original(update); }) as typeof document.startViewTransition;
  });
}
const transitions = (page: Page) => page.evaluate(() => (window as unknown as { __transitions: number }).__transitions);
const supported = (page: Page) => page.evaluate(() => typeof document.startViewTransition === 'function');

test('picker_shows_every_palette_with_a_live_preview', async ({ page }) => {
  await bootSecondary(page, '/settings');
  const group = page.locator('main').getByRole('radiogroup', { name: 'Тема' });
  await expect(group.getByRole('radio')).toHaveCount(PALETTES.length);
  for (const { key, name } of PALETTES) await expect(group.getByRole('radio', { name, exact: true })).toHaveValue(key);
  await expect(group.getByRole('radio', { name: 'Стандартная', exact: true })).toBeChecked();
  // Каждая карточка рисует себя своими токенами: фоны предпросмотров различаются, а не повторяют фон приложения.
  const backgrounds = await group.locator('.preview').evaluateAll(nodes => nodes.map(node => getComputedStyle(node).backgroundColor));
  expect(new Set(backgrounds).size).toBe(PALETTES.length);
  const app = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue('--chat-bg').trim());
  const sepia = group.locator('[data-option=sepia] .preview');
  await expect(sepia).toHaveAttribute('data-theme', 'dark');
  expect(await sepia.evaluate(node => getComputedStyle(node).getPropertyValue('--chat-bg').trim())).not.toBe(app);
  // Карточка — цель нажатия не меньше 44px, предпросмотр не уходит за края на узком экране.
  for (const box of await group.locator('label').evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().toJSON() as DOMRect))) {
    expect(box.height).toBeGreaterThanOrEqual(44);
    expect(box.right).toBeLessThanOrEqual(page.viewportSize()!.width);
  }
  // Предпросмотр следует режиму приложения.
  await page.locator('main').getByRole('radio', { name: 'Светлый', exact: true }).check();
  await expect(sepia).toHaveAttribute('data-theme', 'light');
});

test('arrow_keys_choose_a_palette_and_the_choice_survives_reload', async ({ page }) => {
  await bootSecondary(page, '/settings');
  const group = page.locator('main').getByRole('radiogroup', { name: 'Тема' });
  const first = group.getByRole('radio', { name: 'Стандартная', exact: true });
  await first.focus();
  await page.keyboard.press('ArrowRight');
  await expect(group.getByRole('radio', { name: 'Аквамарин', exact: true })).toBeChecked();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'aquamarine');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await expect(group.getByRole('radio', { name: 'Чернила', exact: true })).toBeChecked();
  await expect(group.getByRole('radio', { name: 'Чернила', exact: true })).toBeFocused();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'ink');
  await page.keyboard.press('ArrowLeft');
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'vscode');
  // Выбор видно не только цветом: у карточки кольцо, у метки заполненный круг, подпись жирнее.
  const chosen = group.locator('[data-option=vscode]');
  const other = group.locator('[data-option=ink]');
  const style = (el: typeof chosen) => el.evaluate(node => ({
    outline: getComputedStyle(node).outlineStyle,
    dot: getComputedStyle(node.querySelector('.mark')!, '::after').inset,
    weight: getComputedStyle(node.querySelector('.name')!).fontWeight
  }));
  const [on, off] = [await style(chosen), await style(other)];
  expect(on.outline).toBe('solid');
  expect(off.outline).toBe('none');
  expect(on.dot).not.toBe(off.dot);
  expect(on.weight).not.toBe(off.weight);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme-switching', '');
  expect(await page.evaluate(() => localStorage.getItem('canterlot.palette'))).toBe('vscode');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'vscode');
  await expect(page.locator('main').getByRole('radio', { name: 'VS Code', exact: true })).toBeChecked();
  const meta = await page.evaluate(() => [document.querySelector('meta[name="theme-color"]')?.getAttribute('content'), getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()]);
  expect(meta[0]).toBe(meta[1]);
});

test('choosing_a_card_reveals_the_theme_through_a_view_transition_when_supported', async ({ page }) => {
  await countTransitions(page);
  await page.addInitScript(() => {
    const target = window as unknown as { __reveal: number[] };
    target.__reveal = [];
    const original = Element.prototype.animate;
    Element.prototype.animate = function (keyframes, options) {
      if (typeof options === 'object' && options?.pseudoElement === '::view-transition-new(root)') target.__reveal.push(Number(options.duration));
      return original.call(this, keyframes, options);
    };
  });
  await bootSecondary(page, '/settings');
  const group = page.locator('main').getByRole('radiogroup', { name: 'Тема' });
  await group.locator('[data-option=forest]').click();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'forest');
  expect(await transitions(page)).toBe(await supported(page) ? 1 : 0);
  // Круг растёт из карточки за длительность токена, а не схлопывается в мгновенную смену.
  if (await supported(page)) await expect.poll(() => page.evaluate(() => (window as unknown as { __reveal: number[] }).__reveal)).toEqual([420]);
  // После перехода служебные атрибуты сняты: переходы цвета снова работают.
  await expect(page.locator('html')).not.toHaveAttribute('data-theme-reveal', '');
  await expect(page.locator('html')).not.toHaveAttribute('data-theme-switching', '');
});

test('reduced_motion_and_motion_off_switch_instantly', async ({ page }) => {
  await countTransitions(page);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await bootSecondary(page, '/settings');
  const main = page.locator('main');
  await main.locator('[data-option=midnight]').click();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'midnight');
  await main.getByRole('radio', { name: 'Светлый', exact: true }).check();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await main.getByRole('radio', { name: 'Выкл', exact: true }).check();
  await expect(page.locator('html')).toHaveAttribute('data-motion', 'off');
  await main.locator('[data-option=lavender]').click();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'lavender');
  expect(await transitions(page)).toBe(0);
});

function block(api: FakeHubApi, seq: number, text: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text, rev: 1, state: 'final' });
  return {
    kind: 'block', id, block_kind: 'text', content: text, content_bytes: text.length, content_truncated: false,
    created_seq: String(seq), created_at: AT, rev: 1, state: 'final', agent: 'agent', model: 'opus', tool_name: null,
    summary: null, parent_block_id: null, input_command_id: null, line_count: text.split('\n').length
  };
}

test('long_chat_keeps_its_scroll_position_across_a_theme_change', async ({ page }) => {
  await countTransitions(page);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Длинная лента' });
  const items: HistoryItem[] = [];
  for (let seq = 1; seq <= 400; seq++) {
    items.push(block(api, seq, seq % 9 === 0 ? '```ts\n' + Array.from({ length: 10 }, (_, line) => `const value${line} = ${line};`).join('\n') + '\n```' : `Ответ ${seq}`));
  }
  api.histories.set(id, items);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const feed = page.locator('.feed');
  // Владелец отлистал вверх: лента не прилипает к низу, и смена темы не должна её сдвинуть.
  await feed.evaluate(el => { el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight * 4); });
  await page.waitForTimeout(400);
  const anchor = () => feed.evaluate(el => {
    const top = el.getBoundingClientRect().top;
    const row = [...el.querySelectorAll<HTMLElement>('[data-row]')].find(node => node.getBoundingClientRect().bottom > top)!;
    return { key: row.dataset.key, offset: Math.round(row.getBoundingClientRect().top - top), scroll: Math.round(el.scrollTop) };
  });
  const before = await anchor();
  const toggle = page.locator('[data-theme-toggle]');
  if (await toggle.isVisible()) await toggle.click();
  else {
    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await page.getByRole('menuitem', { name: 'Светлая тема' }).click();
  }
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('html')).not.toHaveAttribute('data-theme-reveal', '');
  await page.waitForTimeout(200);
  expect(await anchor()).toEqual(before);
  expect(await transitions(page)).toBe(await supported(page) ? 1 : 0);
});
