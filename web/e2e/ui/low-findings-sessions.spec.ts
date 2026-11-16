import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { bootSecondary } from './secondary-fixture';
import { expect, resizeViewport, test } from './fixtures';

// Нижние находки обзора вёрстки 2026-11-16: свёрнутая полоса лимитов с резервом,
// отступы кнопок корзины, вложенные делегаты, строки лимитов на 360, вкладки групп
// в боковой колонке планшета и кнопка закрытия в поповере сессии.
const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const SHOTS = process.env.LOW_SHOTS_DIR;
const phoneOnly = (info: { project: { name: string } }) => test.skip(info.project.name !== 'phone');
const desktopOnly = (info: { project: { name: string } }) => test.skip(info.project.name === 'phone');

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  const theme = await page.locator('html').getAttribute('data-theme');
  await page.screenshot({ path: join(SHOTS, `${name}-${test.info().project.name}-${theme ?? 'dark'}.png`) });
}

const observed = () => new Date(Date.now() - 60_000).toISOString();
const win = (used: number, hours: number) => ({
  used_permille: used,
  resets_at: new Date(Date.now() + hours * 3_600_000).toISOString(),
  observed_at: observed()
});

/** Claude на недельном резерве: в свёрнутой строке шапки рядом с процентами стоит «резерв». */
function reserveLimits(api: FakeHubApi): void {
  api.limits = [
    {
      provider: 'claude',
      weekly_reserve_active: true,
      weekly_reserve_resets_at: new Date(Date.now() + 2 * 86_400_000).toISOString(),
      five_hour: win(610, 4.97),
      weekly: win(955, 47.5)
    },
    { provider: 'codex', weekly_reserve_active: false, five_hour_absent: true, weekly: win(420, 96) }
  ] as never;
}

async function bootChat(page: Page, patch?: (api: FakeHubApi) => void) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  patch?.(api);
  const id = seedConversation(api, { title: 'Лимиты в шапке', activity: { state: 'idle', evidence: { last_turn_at: AT }, since: AT } as never });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 20_000 });
  return page.locator('.chat-header .limits');
}

test.describe('F-23 collapsed limits row with an active reserve', () => {
  for (const width of [360, 412]) {
    test(`the_row_stays_one_line_with_the_reserve_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      const strip = await bootChat(page, reserveLimits);
      await expect(strip.locator('.reserve')).toHaveCount(1);
      await shot(page, `f23-header-reserve-${width}`);
      const found = await strip.evaluate((root) => {
        const box = (node: Element | null) => {
          const r = node!.getBoundingClientRect();
          return { t: r.top, b: r.bottom, h: r.height, l: r.left, r: r.right };
        };
        const reserve = root.querySelector<HTMLElement>('.reserve')!;
        return {
          summary: box(root.querySelector('summary')),
          rows: box(root.querySelector('.rows')),
          reserve: { ...box(reserve), text: reserve.textContent?.trim(), title: reserve.getAttribute('title') },
          five: box(root.querySelector(".meter[data-window='five'] .pct")),
          week: box(root.querySelector(".meter[data-window='week'] .pct"))
        };
      });
      // Одна строка: значение недели стоит на одной высоте с пятичасовым, «резерв» не занимает вторую.
      expect(Math.abs(found.week.t - found.five.t), JSON.stringify(found)).toBeLessThanOrEqual(1);
      expect(found.summary.h, JSON.stringify(found)).toBeLessThanOrEqual(45);
      // Слово остаётся в разметке для чтения с экрана и с подсказкой, но не рисует строку.
      expect(found.reserve.text).toBe('резерв');
      expect(found.reserve.title).toBeTruthy();
      expect(found.reserve.h, '«резерв» не занимает строку').toBeLessThanOrEqual(1);
    });
  }
});

test.describe('F-30 trash buttons keep the pill padding', () => {
  for (const width of [360, 412]) {
    test(`restore_and_purge_have_sixteen_pixels_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      const hub = new FakeHub();
      const api = new FakeHubApi(hub);
      const id = seedConversation(api, { title: 'Сессия для корзины', activity: { state: 'stopped', evidence: {}, since: AT } as never });
      api.trashConversation(id);
      await installChatRoutes(page, hub, api, OWNER);
      await page.goto('/sessions');
      await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
      await page.getByRole('tab', { name: /Корзина/ }).click();
      const buttons = page.locator('.trash button');
      await expect(buttons).toHaveCount(2);
      await shot(page, `f30-trash-buttons-${width}`);
      const found = await buttons.evaluateAll((nodes) => nodes.map((node) => {
        const style = getComputedStyle(node);
        const box = node.getBoundingClientRect();
        const range = document.createRange();
        range.selectNodeContents(node);
        const text = range.getBoundingClientRect();
        return { left: box.left, right: box.right, radius: parseFloat(style.borderTopLeftRadius), pad: parseFloat(style.paddingInlineStart), gap: text.left - box.left };
      }));
      expect(found).toHaveLength(2);
      for (const button of found) {
        expect(button.pad, JSON.stringify(found)).toBeCloseTo(16, 0);
        expect(button.gap, JSON.stringify(found)).toBeGreaterThanOrEqual(15.5);
        // Текст начинается за дугой закругления, а кнопка остаётся пилюлей в одну строку.
        expect(button.radius).toBeGreaterThanOrEqual(21);
      }
    });
  }
});

test.describe('F-33 nested delegate rows', () => {
  async function bootTree(page: Page) {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const parent = seedConversation(api, {
      title: 'Оркестратор',
      activity: { state: 'idle', evidence: { last_turn_at: AT }, since: AT } as never
    });
    const child = seedConversation(api, {
      title: 'Первый делегат',
      parent_id: parent,
      spawn_depth: 1,
      working_ms: 13_380_000,
      activity: { state: 'working', evidence: { phase: 'generating' }, since: AT } as never
    });
    const grand = seedConversation(api, {
      title: 'Второй делегат ждёт разрешения',
      parent_id: child,
      spawn_depth: 2,
      working_ms: 1_380_000,
      unread_count: 128,
      queue: { queued: 12, pause_reasons: [], hold_reasons: [] },
      activity: { state: 'waiting_for_user', evidence: { phase: 'awaiting_permission', question: 'Bash: rm -rf target/debug/incremental && cargo build --release' }, since: AT } as never
    });
    api.counts = api.recount();
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    // Раскрываем ветки, пока не покажутся все три уровня.
    for (let round = 0; round < 4; round++) {
      const folds = page.locator('button[aria-expanded="false"]').filter({ hasText: /Делегированные/ });
      if ((await folds.count()) === 0) break;
      await folds.first().click({ timeout: 2_000 }).catch(() => {});
    }
    await expect(page.locator(`[data-session="${grand}"]`)).toHaveClass(/depth-2/);
    return { grand };
  }

  for (const width of [360, 412]) {
    test(`row_and_fold_edges_form_one_column_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      const { grand } = await bootTree(page);
      await shot(page, `f33-nested-delegates-${width}`);
      const found = await page.evaluate(() => {
        const round = (n: number) => Math.round(n * 10) / 10;
        const rows = [...document.querySelectorAll<HTMLElement>('article.row')].map((row) => {
          const button = row.querySelector<HTMLElement>(':scope > button')!;
          return { depth: row.classList.contains('depth-2') ? 2 : row.classList.contains('child') ? 1 : 0, rowRight: round(row.getBoundingClientRect().right), moreRight: round(button.getBoundingClientRect().right) };
        });
        const folds = [...document.querySelectorAll<HTMLElement>('button.kids')].map((fold) => ({
          depth: fold.classList.contains('depth-2') ? 2 : 1,
          right: round(fold.getBoundingClientRect().right),
          chevronRight: round(fold.querySelector<HTMLElement>('.chevron')!.getBoundingClientRect().right)
        }));
        return { rows, folds };
      });
      // Строки всех трёх уровней кончаются на одном правом краю: 6 px на уровень больше не набегают.
      const expected = found.rows.find((row) => row.depth === 0)!.rowRight;
      for (const row of found.rows) {
        expect(Math.abs(row.rowRight - expected), JSON.stringify(found.rows)).toBeLessThanOrEqual(1);
        expect(Math.abs(row.moreRight - expected), JSON.stringify(found.rows)).toBeLessThanOrEqual(1);
      }
      expect(found.folds.length).toBeGreaterThanOrEqual(2);
      for (const fold of found.folds) expect(Math.abs(fold.right - expected), JSON.stringify(found.folds)).toBeLessThanOrEqual(1);
      // Стрелки сгибов на всех уровнях стоят одной колонкой.
      const chevron = found.folds[0]!.chevronRight;
      for (const fold of found.folds) expect(Math.abs(fold.chevronRight - chevron), JSON.stringify(found.folds)).toBeLessThanOrEqual(1);

      // У самого узкого ожидающего ряда на 412 px хватает места на «Ждёт разрешения» целиком.
      if (width === 412) {
        const state = await page.locator(`[data-session="${grand}"] .activity .text`).evaluate((node) => {
        const box = node.getBoundingClientRect();
        const data = node.firstChild as Text;
        const phrase = 'Ждёт разрешения';
        const range = document.createRange();
        range.setStart(data, 0);
        range.setEnd(data, phrase.length);
        const rects = [...range.getClientRects()].filter((rect) => rect.width > 0);
        return {
          text: (node.textContent ?? '').trim(),
          clientWidth: node.clientWidth,
          scrollWidth: node.scrollWidth,
          right: Math.max(...rects.map((rect) => rect.right)),
          limit: box.left + node.clientWidth
        };
      });
        expect(state.text.startsWith('Ждёт разрешения')).toBe(true);
        expect(state.scrollWidth, JSON.stringify(state)).toBeGreaterThan(state.clientWidth);
        expect(state.right, JSON.stringify(state)).toBeLessThanOrEqual(state.limit + 0.5);
      }
    });
  }
});

test.describe('F-35 current limits rows at 360', () => {
  test('value_and_reset_do_not_overlap_with_three_windows', async ({ page }, info) => {
    phoneOnly(info);
    await page.setViewportSize({ width: 360, height: 892 });
    const now = Date.now();
    const at = (ms: number) => new Date(now + ms).toISOString();
    await bootSecondary(page, '/limits', (api) => {
      api.limits = [
        {
          provider: 'claude',
          weekly_reserve_active: true,
          weekly_reserve_resets_at: at(2 * 86_400_000),
          five_hour: { used_permille: 610, resets_at: at(4 * 3_600_000 + 58 * 60_000), observed_at: at(-60_000) },
          weekly: { used_permille: 950, resets_at: at(47 * 3_600_000 + 29 * 60_000), observed_at: at(-60_000) },
          weekly_fable: { used_permille: 870, resets_at: at(47 * 3_600_000 + 29 * 60_000), observed_at: at(-60_000) }
        }
      ] as never;
    });
    const card = page.getByRole('region', { name: 'Claude, текущие лимиты' });
    await expect(card.locator('.row')).toHaveCount(3);
    await shot(page, 'f35-limits-rows-360');
    for (const width of [360, 390]) {
      await resizeViewport(page, { width, height: 892 });
      const rows = await card.locator('.row').evaluateAll((nodes) => nodes.map((row) => {
        const ink = (node: Element | null) => {
          if (!node) return null;
          const range = document.createRange();
          range.selectNodeContents(node);
          const rects = [...range.getClientRects()].filter((rect) => rect.width > 0);
          if (rects.length === 0) return null;
          return {
            left: Math.min(...rects.map((rect) => rect.left)),
            right: Math.max(...rects.map((rect) => rect.right)),
            top: Math.min(...rects.map((rect) => rect.top)),
            bottom: Math.max(...rects.map((rect) => rect.bottom))
          };
        };
        const value = ink(row.querySelector('.value'));
        const resetNode = row.querySelector<HTMLElement>('.reset');
        if (!value || !resetNode) return { label: row.querySelector('.label')?.textContent?.trim(), value, reset: null, visual: null };
        const box = resetNode.getBoundingClientRect();
        // В тесной карточке текст сброса рисует псевдоэлемент: в разметке остаётся полная подпись.
        const long = row.querySelector<HTMLElement>('.reset-long')!;
        const visual = getComputedStyle(long).display === 'none'
          ? getComputedStyle(resetNode, '::after').content
          : (long.textContent ?? '');
        return {
          label: row.querySelector('.label')?.textContent?.trim(),
          value,
          reset: { left: box.left, right: box.right, top: box.top, bottom: box.bottom, text: resetNode.textContent!.trim() },
          visual
        };
      }));
      expect(rows).toHaveLength(3);
      for (const row of rows) {
        expect(row.reset).not.toBeNull();
        expect(row.reset!.text).toMatch(/^сброс через\s+\d+/);
        expect(row.visual).toMatch(/\d+\s*ч\s*\d+\s*м/);
        const overlapX = Math.min(row.value!.right, row.reset!.right) - Math.max(row.value!.left, row.reset!.left);
        const overlapY = Math.min(row.value!.bottom, row.reset!.bottom) - Math.max(row.value!.top, row.reset!.top);
        expect(overlapX <= 0.5 || overlapY <= 0.5, `${width}px ${row.label}: ${JSON.stringify(row)}`).toBe(true);
      }
    }
  });
});

test.describe('F-38 group tabs in the tablet sidebar', () => {
  test('eleven_long_groups_take_at_most_two_rows', async ({ page }, info) => {
    desktopOnly(info);
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    seedMockupSessions(api);
    for (let index = 1; index <= 11; index++) {
      const group = api.id();
      api.groups.push({ id: group, name: `Инфраструктура и выкатка homelab ${index}`, position: api.groups.length, created_at: AT });
      seedConversation(api, { title: `Сессия группы ${index}`, groups: [group] });
    }
    api.counts = api.recount();
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await resizeViewport(page, { width: 1024, height: 768 });
    const tabs = page.getByRole('tablist', { name: 'Группы сессий' });
    await expect(tabs).toBeVisible();
    await shot(page, 'f38-group-tabs-1024');
    const found = await tabs.evaluate((root) => {
      const box = root.getBoundingClientRect();
      const style = getComputedStyle(root);
      const items = [...root.children].map((child) => {
        const r = child.getBoundingClientRect();
        const own = getComputedStyle(child);
        return { text: child.textContent?.trim() ?? '', top: r.top, bottom: r.bottom, left: r.left, right: r.right, height: r.height, overflow: own.textOverflow, wrap: own.whiteSpace };
      });
      const scroll = document.querySelector('[data-list-scroll]')!.getBoundingClientRect();
      const pane = document.querySelector('[data-pane="list"]')!.getBoundingClientRect();
      const visible = items.filter((item) => item.top >= box.top - 0.5 && item.bottom <= box.bottom + 0.5);
      return {
        box: { top: box.top, bottom: box.bottom, height: box.height, left: box.left, right: box.right, scrollWidth: root.scrollWidth, clientWidth: root.clientWidth },
        gap: parseFloat(style.rowGap || style.gap) || 0,
        padTop: parseFloat(style.paddingTop),
        padBottom: parseFloat(style.paddingBottom),
        rows: [...new Set(visible.map((item) => Math.round(item.top)))].length,
        rowHeight: items[0]?.height ?? 0,
        items,
        visible: visible.length,
        scrollHeight: scroll.height,
        paneHeight: pane.height
      };
    });
    // Не больше двух рядов вкладок: высота полосы считается по двум кнопкам, зазор и отступы.
    const twoRows = found.rowHeight * 2 + found.gap + found.padTop + found.padBottom;
    expect(found.box.height, JSON.stringify(found)).toBeLessThanOrEqual(twoRows + 1);
    expect(found.rows, JSON.stringify(found)).toBeLessThanOrEqual(2);
    expect(found.visible).toBeGreaterThan(0);
    // Список сессий сохраняет большую часть боковой колонки.
    expect(found.scrollHeight, JSON.stringify(found)).toBeGreaterThan(found.paneHeight * 0.55);
    // Полоса не прокручивается вбок, а длинные имена заканчиваются многоточием.
    expect(found.box.scrollWidth, JSON.stringify(found.box)).toBeLessThanOrEqual(found.box.clientWidth + 1);
    for (const item of found.items) {
      expect(item.left).toBeGreaterThanOrEqual(found.box.left - 0.5);
      expect(item.right).toBeLessThanOrEqual(found.box.right + 0.5);
      expect(item.overflow).toBe('ellipsis');
      expect(item.wrap).toBe('nowrap');
    }
  });
});
