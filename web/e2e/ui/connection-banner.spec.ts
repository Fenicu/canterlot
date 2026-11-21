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

test('connection_banner_gets_its_own_row_above_the_sessions_list', async ({ page }, testInfo) => {
  const phone = testInfo.project.name === 'phone';
  const { hub } = await boot(page);
  const list = page.locator('[data-list-scroll]');
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  await arrived(page);
  const card = (await banner(page).boundingBox())!;
  if (phone) {
    // Баннер не лежит поверх списка: он занимает свою полосу над ним,
    // поэтому первая секция и её заголовок остаются видны.
    const scroller = (await list.boundingBox())!;
    expect(card.y + card.height).toBeLessThanOrEqual(scroller.y + 1);
    expect(await banner(page).evaluate((node) => getComputedStyle(node.parentElement!).position)).toBe('static');
  } else {
    // Стол: баннер стоит в панели чата, списка он не касается.
    const pane = (await page.locator('[data-pane="list"]').boundingBox())!;
    expect(card.x).toBeGreaterThanOrEqual(pane.x + pane.width - 1);
  }
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
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
  const { hub, api, ids } = await boot(page);
  await openChat(page, api, ids.pinned);
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
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
});

const mark = (page: Page) => banner(page).locator('svg.mark');

declare global {
  interface Window {
    __animated?: { cls: string; props: string[] }[];
  }
}

/** Вызовы `element.animate` с классом и свойствами: видно, где слово сменилось с движением, а где сразу. */
async function recordAnimations(page: Page) {
  await page.addInitScript(() => {
    const log: NonNullable<Window['__animated']> = [];
    window.__animated = log;
    const original = Element.prototype.animate;
    Element.prototype.animate = function (this: Element, keyframes: Keyframe[] | PropertyIndexedKeyframes | null, options?: number | KeyframeAnimationOptions) {
      const frames = Array.isArray(keyframes) ? keyframes : [];
      log.push({ cls: this.getAttribute('class') ?? '', props: [...new Set(frames.flatMap((frame) => Object.keys(frame)))] });
      return original.call(this, keyframes as Keyframe[], options);
    };
  });
}

const swaps = (page: Page) => page.evaluate(() => window.__animated!.filter((entry) => entry.cls.split(' ').includes('words')).length);

test('connection_banner_sketches_wifi_off_while_offline_and_inks_wifi_on_return', async ({ page }) => {
  const { icons } = await import('../../src/lib/ui/icons');
  await recordAnimations(page);
  const { hub } = await boot(page);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  await expect(mark(page)).toHaveAttribute('data-state', 'sketch');
  await expect(mark(page).locator('.ghost')).toHaveAttribute('d', icons.wifiOff);
  // Набросок дышит, пока связи нет.
  expect(await mark(page).locator('.ghost').evaluate((node) => node.getAnimations().map((item) => (item as CSSAnimation).animationName.replace(/^.*-(?=mark-breathe$)/, '')))).toEqual(['mark-breathe']);
  const offlineText = await swaps(page);
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  // Окно «Связь восстановлена» две секунды: всё читается за один заход в страницу.
  const restored = await page.evaluate(() => {
    const node = document.querySelector('[data-banner] svg.mark')!;
    const probe = document.createElement('span');
    probe.style.color = 'var(--success)';
    document.body.append(probe);
    const success = getComputedStyle(probe).color;
    probe.remove();
    return {
      state: node.getAttribute('data-state'),
      path: node.querySelector('.ghost')!.getAttribute('d'),
      color: getComputedStyle(node).color,
      success,
      svgs: document.querySelectorAll('[data-banner] svg').length,
      swaps: window.__animated!.filter((entry) => entry.cls.split(' ').includes('words')).length
    };
  });
  expect(restored.state).toBe('ink');
  expect(restored.path).toBe(icons.wifi);
  expect(restored.color).toBe(restored.success);
  // Один значок связи вместо общего значка предупреждения, а слово меняется с движением.
  expect(restored.svgs).toBe(1);
  expect(restored.swaps).toBeGreaterThan(offlineText);
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
});

test('connection_banner_words_swap_in_place_and_stand_still_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await recordAnimations(page);
  const { hub } = await boot(page);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  // Без движения набросок стоит ровно, а слова не двигаются.
  expect(await mark(page).locator('.ghost').evaluate((node) => node.getAnimations().length)).toBe(0);
  hub.acceptConnections = true;
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 20_000 });
  expect(await mark(page).evaluate((node) => node.getAnimations({ subtree: true }).length)).toBe(0);
  expect(await swaps(page)).toBe(0);
  await expect(banner(page)).toHaveCount(0, { timeout: 5_000 });
});
