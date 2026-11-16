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
