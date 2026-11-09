import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

declare global {
  interface Window {
    bannerSentinel?: number;
  }
}

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

async function openChat(page: Page, api: FakeHubApi, id: string) {
  api.bindConversation(id);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.chat-header')).toBeVisible({ timeout: 15_000 });
}

const top = async (locator: Locator) => {
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  return Math.round(box!.y);
};

/** Положение всего, что лежит под баннером: оно не должно сдвигаться ни на пиксель. */
const positions = (targets: Locator[]) => Promise.all(targets.map(top));

/** Экран успевает дорисовать полосу лимитов и вкладки: замер берётся, когда два чтения подряд совпали. */
async function settled(page: Page, targets: Locator[]) {
  let previous = await positions(targets);
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.waitForTimeout(250);
    const next = await positions(targets);
    if (JSON.stringify(next) === JSON.stringify(previous)) return next;
    previous = next;
  }
  return previous;
}

const banner = (page: Page) => page.locator('[data-banner]');

/** Карточка въезжает сверху: её место меряется, когда сдвиг появления доехал до конца. */
const arrived = (page: Page) =>
  banner(page).evaluate((card) => Promise.all(card.getAnimations().map((animation) => animation.finished)).then(() => undefined));

async function cutAndRestore(page: Page, hub: FakeHub, targets: Locator[]) {
  const before = await settled(page, targets);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  expect(await positions(targets)).toEqual(before);
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  expect(await positions(targets)).toEqual(before);
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
  expect(await positions(targets)).toEqual(before);
}

test('connection_banner_floats_over_the_sessions_list_without_moving_it', async ({ page }) => {
  const { hub } = await boot(page);
  const list = page.locator('[data-list-scroll]');
  const row = page.locator('[data-list-scroll] a').first();
  await cutAndRestore(page, hub, [list, row]);
});

test('connection_banner_floats_over_a_chat_and_keeps_the_header_free', async ({ page }) => {
  const { hub, api, ids } = await boot(page);
  await openChat(page, api, ids.pinned);
  const header = page.locator('.chat-header');
  const composer = page.getByRole('textbox').last();
  const before = await settled(page, [header, composer]);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  expect(await positions([header, composer])).toEqual(before);
  // Карточка лежит под шапкой: кнопки «назад» и меню остаются доступны.
  await arrived(page);
  const head = (await header.boundingBox())!;
  const card = (await banner(page).boundingBox())!;
  expect(card.y).toBeGreaterThanOrEqual(head.y + head.height - 1);
  const composerBox = (await composer.boundingBox())!;
  expect(card.y + card.height).toBeLessThan(composerBox.y);
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  expect(await positions([header, composer])).toEqual(before);
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
  expect(await positions([header, composer])).toEqual(before);
});

test('connection_banner_lets_taps_through_outside_the_card', async ({ page }) => {
  const { hub } = await boot(page);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  const probe = await banner(page).evaluate((card) => {
    const box = card.getBoundingClientRect();
    const layer = card.parentElement!;
    const y = box.top + box.height / 2;
    const beside = document.elementFromPoint(Math.max(1, box.left - 4), y);
    const below = document.elementFromPoint(box.left + box.width / 2, box.bottom + 6);
    return {
      layer: getComputedStyle(layer).pointerEvents,
      card: getComputedStyle(card).pointerEvents,
      position: getComputedStyle(layer).position,
      besideIsBanner: !!beside && layer.contains(beside),
      belowIsBanner: !!below && layer.contains(below),
      onCard: card.contains(document.elementFromPoint(box.left + box.width / 2, y))
    };
  });
  expect(probe).toEqual({ layer: 'none', card: 'auto', position: 'absolute', besideIsBanner: false, belowIsBanner: false, onCard: true });
  hub.acceptConnections = true;
});

test('connection_banner_action_still_runs_from_the_overlay', async ({ page }) => {
  const { hub } = await boot(page);
  await page.evaluate(() => { window.bannerSentinel = 1; });
  for (let attempt = 0; attempt < 3; attempt += 1) {
    hub.chaos.forceResync('cursor_ahead');
    await page.waitForTimeout(1000);
  }
  await expect(banner(page)).toContainText('Не удаётся синхронизироваться. Повторяю попытки', { timeout: 8_000 });
  await banner(page).getByRole('button', { name: 'Обновить' }).click();
  await expect.poll(() => page.evaluate(() => window.bannerSentinel).catch(() => 1), { timeout: 15_000 }).toBeUndefined();
});

test('connection_banner_appears_without_a_slide_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const { hub } = await boot(page);
  const list = page.locator('[data-list-scroll]');
  const [before] = await settled(page, [list]);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  // Каждый кадр появления: карточка не едет, меняется только прозрачность.
  const shifts = await banner(page).evaluate((card) => new Promise<string[]>((resolve) => {
    const seen: string[] = [];
    let frames = 0;
    const step = () => {
      seen.push(getComputedStyle(card).transform);
      if (++frames < 10) requestAnimationFrame(step);
      else resolve(seen);
    };
    step();
  }));
  expect(shifts.every((value) => value === 'none')).toBe(true);
  expect(await top(list)).toBe(before);
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
  expect(await top(list)).toBe(before);
});
