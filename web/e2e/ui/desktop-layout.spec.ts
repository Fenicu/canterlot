import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { bootScreen, SCREENS, type Theme } from '../parity/states';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

// Безголовый Firefox в Playwright прячет полосы прокрутки агентским `scrollbar-width: none !important`,
// поэтому проверки ловят сам перелив (scrollWidth), а не видимую полосу.
const OWNER = '00000000-0000-4000-8000-0000000000d1';
const THEMES: Theme[] = ['dark', 'light'];
// DejaVu Sans шире Noto Sans из живого Firefox владельца: если вмещается он, вмещается и стек --font-sans.
const WIDE_FONT = '"DejaVu Sans", sans-serif';
const EXPECTED: Record<string, { body: number; sidebar: [number, number] }> = {
  'layout-1024': { body: 15, sidebar: [0.25, 0.34] },
  'layout-1440': { body: 15, sidebar: [0.2, 0.24] },
  'layout-2560': { body: 18, sidebar: [0.16, 0.2] },
  'layout-3840': { body: 24, sidebar: [0.16, 0.2] }
};

const named = (name: string) => SCREENS.find((item) => item.name === name)!;
const size = (page: Page) => page.viewportSize()!;
const list = (page: Page) => page.locator('[data-pane="list"]');
const pane = (page: Page) => page.locator('[data-pane="chat"]');
const widenFont = (page: Page) => page.evaluate((font) => document.documentElement.style.setProperty('--font-sans', font), WIDE_FONT);

async function bootEmpty(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
}

async function bootLongGroup(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  api.groups.push({ id: api.id(), name: 'Очень длинное название группы для проверки переноса вкладок в боковой панели', position: 3, created_at: '2026-10-28T12:00:00.000Z' });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
}

const bootSessions = (page: Page, theme?: Theme) => bootScreen(page, named('01-sessions'), theme, size(page));
const bootNew = (page: Page, theme?: Theme) => bootScreen(page, named('07-new-session'), theme, size(page));
const bootChat = (page: Page, theme?: Theme) => bootScreen(page, named('02-chat'), theme, size(page));

function titleClashes(page: Page): Promise<string[]> {
  return list(page).getByText('canterlot', { exact: true }).first().evaluate((title) => {
    const range = document.createRange();
    range.selectNodeContents(title);
    const text = range.getBoundingClientRect();
    const clashes = title.scrollWidth > title.clientWidth ? [`clipped ${title.scrollWidth} > ${title.clientWidth}`] : [];
    for (const control of title.parentElement!.querySelectorAll('a, button')) {
      const box = control.getBoundingClientRect();
      if (box.right > text.left && box.left < text.right && box.bottom > text.top && box.top < text.bottom) clashes.push(control.getAttribute('aria-label') ?? '');
    }
    return clashes;
  });
}

function tabsProblems(page: Page): Promise<string[]> {
  return page.getByRole('tablist', { name: 'Группы сессий' }).evaluate((tabs) => {
    const box = tabs.getBoundingClientRect();
    const problems = tabs.scrollWidth > tabs.clientWidth ? [`overflow ${tabs.scrollWidth} > ${tabs.clientWidth}`] : [];
    for (const tab of tabs.children) {
      const rect = tab.getBoundingClientRect();
      if (rect.left < box.left - 0.5 || rect.right > box.right + 0.5 || rect.top < box.top - 0.5 || rect.bottom > box.bottom + 0.5) problems.push(`clipped ${tab.textContent?.trim()}`);
    }
    return problems;
  });
}

async function offsets(element: Locator, container: Locator) {
  const host = await container.elementHandle();
  return element.evaluate((node, outerNode) => {
    const inner = node.getBoundingClientRect();
    const outer = outerNode!.getBoundingClientRect();
    return { left: inner.left - outer.left, right: outer.right - inner.right, top: inner.top - outer.top, bottom: outer.bottom - inner.bottom, width: inner.width };
  }, host);
}

async function expectCentred(element: Locator, container: Locator) {
  const gaps = await offsets(element, container);
  expect(gaps.left, 'слева внутри панели').toBeGreaterThanOrEqual(0);
  expect(gaps.right, 'справа внутри панели').toBeGreaterThanOrEqual(0);
  expect(Math.abs(gaps.left - gaps.right), 'по центру панели').toBeLessThanOrEqual(2);
  return gaps;
}

function tokenPx(page: Page, token: string): Promise<number> {
  return page.evaluate((name) => {
    const probe = document.createElement('div');
    probe.style.position = 'fixed';
    probe.style.width = `var(${name})`;
    document.body.append(probe);
    const width = probe.getBoundingClientRect().width;
    probe.remove();
    return width;
  }, token);
}

function overflowing(page: Page) {
  return page.evaluate(() => {
    const doc = document.documentElement;
    const wide = [...document.querySelectorAll<HTMLElement>('#app-shell *')]
      .filter((el) => !el.closest('pre, code') && ['auto', 'scroll'].includes(getComputedStyle(el).overflowX) && el.scrollWidth > el.clientWidth + 1)
      .map((el) => `${el.tagName.toLowerCase()}.${el.className}: ${el.scrollWidth} > ${el.clientWidth}`);
    return { wide, width: doc.scrollWidth - doc.clientWidth, height: doc.scrollHeight - doc.clientHeight };
  });
}

function plainButtons(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const clear = (color: string) => color === 'transparent' || color === 'rgba(0, 0, 0, 0)';
    const painted = (el: Element | null): string => {
      for (let node = el; node; node = node.parentElement) {
        const color = getComputedStyle(node).backgroundColor;
        if (!clear(color)) return color;
      }
      return '';
    };
    const probe = document.createElement('div');
    probe.style.color = 'var(--accent-link)';
    document.body.append(probe);
    const link = getComputedStyle(probe).color;
    probe.remove();
    return [...document.querySelectorAll<HTMLElement>('#app-shell button')].filter((button) => {
      const box = button.getBoundingClientRect();
      if (box.width === 0 || box.height === 0) return false;
      // Документированные исключения: круглые иконки IconButton, вкладки, пункты меню, строки-раскрывашки, вариант text и текстовое действие цвета ссылки.
      if (button.querySelector('svg') || button.matches('[role="tab"], [role="menuitem"], [aria-expanded], .text-button')) return false;
      const style = getComputedStyle(button);
      if (clear(style.backgroundColor)) return style.color !== link;
      return style.backgroundColor === painted(button.parentElement);
    }).map((button) => `«${button.textContent?.trim()}»`);
  });
}

test('sidebar_header_keeps_the_title_clear_of_its_buttons', async ({ page }) => {
  await bootEmpty(page);
  expect(await titleClashes(page)).toEqual([]);
  await widenFont(page);
  expect(await titleClashes(page)).toEqual([]);
});

test('group_tabs_stay_inside_the_sidebar_without_a_scrollbar', async ({ page }) => {
  await bootEmpty(page);
  expect(await tabsProblems(page)).toEqual([]);
  await widenFont(page);
  expect(await tabsProblems(page)).toEqual([]);
  await bootLongGroup(page);
  expect(await tabsProblems(page)).toEqual([]);
  await widenFont(page);
  expect(await tabsProblems(page)).toEqual([]);
});

test('sidebar_navigation_stays_on_one_row', async ({ page }) => {
  await bootEmpty(page);
  await widenFont(page);
  const tops = await list(page).getByRole('navigation', { name: 'Навигация' }).getByRole('link').evaluateAll((links) => links.map((link) => link.getBoundingClientRect().top));
  expect(new Set(tops).size).toBe(1);
});

test('search_field_explains_itself_and_keeps_reset_inside_the_pill', async ({ page }) => {
  await bootEmpty(page);
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  const field = page.getByRole('textbox', { name: 'Поиск сессий' });
  await expect(field).toHaveAttribute('placeholder', 'Поиск сессий');
  const reset = list(page).getByRole('button', { name: 'Сбросить' });
  await expect(reset).toHaveCount(0);
  await field.fill('поток');
  await expect(reset).toBeVisible();
  const pill = page.getByRole('search');
  await expect(pill.locator('input')).toHaveCount(1);
  expect((await offsets(reset, pill)).right).toBeGreaterThanOrEqual(-0.5);
  expect(await pill.evaluate((node) => getComputedStyle(node).backgroundColor)).not.toBe('rgba(0, 0, 0, 0)');
  await field.press('Escape');
  await expect(reset).toHaveCount(0);
  await field.fill('поток');
  await reset.click();
  await expect(reset).toHaveCount(0);
  await expect(field).toBeFocused();
});

test('new_session_form_has_no_empty_bars_and_stays_centred_in_its_pane', async ({ page }) => {
  await bootEmpty(page);
  await page.goto('/new');
  const form = page.getByRole('dialog', { name: 'Новая сессия' });
  await expect(form.getByRole('combobox', { name: 'Машина' })).toBeVisible();
  await expect(form.getByRole('radiogroup')).toHaveCount(0);
  const empty = await form.evaluate((root) => [...root.querySelectorAll('*')]
    .filter((el) => !el.closest('label') && el.children.length === 0 && !el.textContent?.trim() && getComputedStyle(el).backgroundColor !== 'rgba(0, 0, 0, 0)' && el.getBoundingClientRect().height > 0)
    .map((el) => `${el.tagName.toLowerCase()}.${el.className}`));
  expect(empty).toEqual([]);
  await expect(form.getByRole('textbox', { name: 'Подпуть' })).toHaveAttribute('placeholder', 'подпуть');
  const gaps = await expectCentred(form, pane(page));
  expect(gaps.width).toBeLessThanOrEqual((await tokenPx(page, '--phone-max')) + 0.5);
  expect(await form.evaluate((node) => node.scrollWidth - node.clientWidth)).toBeLessThanOrEqual(0);
});

test('create_button_is_a_button_when_disabled_and_fills_the_form_row', async ({ page }) => {
  for (const theme of THEMES) {
    await bootNew(page, theme);
    const form = page.getByRole('dialog', { name: 'Новая сессия' });
    const create = form.getByRole('button', { name: 'Создать' });
    await expect(create).toBeDisabled();
    const look = await create.evaluate((button) => {
      const own = getComputedStyle(button);
      return { background: own.backgroundColor, form: getComputedStyle(button.closest('dialog')!).backgroundColor, width: button.getBoundingClientRect().width, height: button.getBoundingClientRect().height };
    });
    expect(look.background).not.toBe('rgba(0, 0, 0, 0)');
    expect(look.background, theme).not.toBe(look.form);
    expect(look.height).toBeGreaterThanOrEqual((await tokenPx(page, '--target')) - 0.5);
    const inner = await form.evaluate((node) => { const style = getComputedStyle(node); return node.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight); });
    expect(Math.abs(look.width - inner)).toBeLessThanOrEqual(1);
  }
});

test('short_window_reaches_the_create_button_inside_the_pane', async ({ page }) => {
  await page.setViewportSize({ width: size(page).width, height: 600 });
  await bootNew(page);
  const create = page.getByRole('dialog', { name: 'Новая сессия' }).getByRole('button', { name: 'Создать' });
  await create.scrollIntoViewIfNeeded();
  await expect(create).toBeInViewport({ ratio: 1 });
  expect((await overflowing(page)).height).toBeLessThanOrEqual(0);
});

test('every_button_looks_like_a_button_in_both_themes', async ({ page }) => {
  test.setTimeout(120_000);
  for (const theme of THEMES) {
    for (const boot of [bootSessions, bootNew, bootChat]) {
      await boot(page, theme);
      expect(await plainButtons(page), theme).toEqual([]);
    }
  }
});

test('type_and_panes_scale_with_the_viewport', async ({ page }, testInfo) => {
  const expected = EXPECTED[testInfo.project.name]!;
  await bootEmpty(page);
  const measure = () => page.evaluate(() => ({
    body: parseFloat(getComputedStyle(document.body).fontSize),
    root: parseFloat(getComputedStyle(document.documentElement).fontSize),
    sidebar: document.querySelector('[data-pane="list"]')!.getBoundingClientRect().width,
    stageBox: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--icon-small'))
  }));
  const wide = await measure();
  expect(wide.body).toBeCloseTo(expected.body, 1);
  expect(wide.stageBox, 'StageTrack читает токен числом').toBeCloseTo(16 * (expected.body / 15), 0);
  expect(wide.sidebar / size(page).width).toBeGreaterThanOrEqual(expected.sidebar[0]);
  expect(wide.sidebar / size(page).width).toBeLessThanOrEqual(expected.sidebar[1]);
  expect(wide.sidebar).toBeGreaterThanOrEqual(20 * wide.root - 0.5);
  expect(wide.sidebar).toBeLessThanOrEqual(28 * wide.root + 0.5);
  // Снимок владельца: 3839 физических точек при масштабе 1,5 дают окно чуть уже 2560 CSS-пикселей.
  await page.setViewportSize({ width: size(page).width - 1, height: Math.round(size(page).height * 0.89) });
  await expect.poll(async () => (await measure()).body).toBeCloseTo(expected.body, 1);
  await expect(page.getByText('Выберите сессию')).toBeVisible();
  await expectCentred(page.getByText('Выберите сессию'), pane(page));
});

test('chat_column_keeps_the_reading_measure_and_stays_centred', async ({ page }) => {
  await bootChat(page);
  const reading = await tokenPx(page, '--reading-max');
  for (const part of [page.locator('.topbar'), page.locator('[data-row]').first(), page.locator('.capsule')]) {
    const gaps = await expectCentred(part, pane(page));
    expect(gaps.width).toBeLessThanOrEqual(reading + 0.5);
  }
  const scrollbarAtPaneEdge = await offsets(page.getByRole('log'), pane(page));
  expect(scrollbarAtPaneEdge.right).toBeLessThanOrEqual(1);
});

test('no_screen_scrolls_the_document_or_overflows_sideways', async ({ page }) => {
  test.setTimeout(120_000);
  const check = async (label: string) => {
    const found = await overflowing(page);
    expect(found.wide, label).toEqual([]);
    expect(found.width, label).toBeLessThanOrEqual(0);
    expect(found.height, label).toBeLessThanOrEqual(0);
  };
  await bootSessions(page);
  await check('/sessions');
  await bootNew(page);
  await check('/new');
  await bootChat(page);
  await check('/c');
  for (const path of ['/machines', '/limits', '/presets', '/settings']) {
    await bootSecondary(page, path);
    await page.waitForTimeout(150);
    await check(path);
  }
});
