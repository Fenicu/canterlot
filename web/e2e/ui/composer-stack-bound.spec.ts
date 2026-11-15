import type { Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup08, seedMockupChat } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

/** Ниже этого лента не должна схлопываться: иначе от переписки остаётся полоска. */
const FEED_FLOOR = 48;

// Четыре состояния владельца: узкий телефон, обычный телефон, телефон с клавиатурой и ландшафт.
const VIEWPORTS = [
  { width: 360, height: 740 },
  { width: 412, height: 892 },
  { width: 412, height: 450 },
  { width: 892, height: 412 }
];

const SHOTS = process.env.F01_SHOTS;

interface Box {
  top: number;
  bottom: number;
  height: number;
}

interface Metrics {
  viewport: number;
  composer: Box;
  send: Box | null;
  field: Box | null;
  feed: Box;
  stack: Box;
  overflowY: string;
  scrollHeight: number;
  clientHeight: number;
  last: Box | null;
}

async function boot(page: Page, seed: (api: FakeHubApi) => string): Promise<{ hub: FakeHub; api: FakeHubApi; id: string }> {
  await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'dark'));
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seed(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

/** Один замер отдаёт всё, что нужно проверке: рамки, прокрутку и последний элемент. */
async function measure(page: Page, stack: string, item: string): Promise<Metrics> {
  return page.evaluate(
    ({ stack: stackSelector, item: itemSelector }) => {
      const box = (el: Element): Box => {
        const rect = el.getBoundingClientRect();
        return { top: rect.top, bottom: rect.bottom, height: rect.height };
      };
      const one = (selector: string): Element => {
        const el = document.querySelector(selector);
        if (!el) throw new Error(`нет элемента ${selector}`);
        return el;
      };
      const container = one(stackSelector);
      const items = document.querySelectorAll(itemSelector);
      const last = items.length > 0 ? items[items.length - 1]! : null;
      return {
        viewport: window.innerHeight,
        composer: box(one('section.composer')),
        send: document.querySelector('section.composer button.send') ? box(document.querySelector('section.composer button.send')!) : null,
        field: document.querySelector('section.composer textarea') ? box(document.querySelector('section.composer textarea')!) : null,
        feed: box(one('.feed')),
        stack: box(container),
        overflowY: getComputedStyle(container).overflowY,
        scrollHeight: container.scrollHeight,
        clientHeight: container.clientHeight,
        last: last ? box(last) : null
      };
    },
    { stack, item }
  );
}

/** Скриншоты до и после правки: F01_SHOTS=before|after. */
async function shoot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  const width = page.viewportSize()!.width;
  const height = page.viewportSize()!.height;
  const project = test.info().project.name;
  mkdirSync('/tmp/f01-composer-stack', { recursive: true });
  await page.screenshot({ path: `/tmp/f01-composer-stack/${SHOTS}-${project}-${name}-${width}x${height}.png` });
}

/** Поле ввода и «Отправить» целиком в окне: владелец может печатать и отправить. */
function expectComposerReachable(metrics: Metrics): void {
  expect(metrics.composer.top).toBeGreaterThanOrEqual(-0.5);
  expect(metrics.composer.bottom).toBeLessThanOrEqual(metrics.viewport + 0.5);
  expect(metrics.send, 'кнопка «Отправить» есть в поле ввода').not.toBeNull();
  expect(metrics.send!.top).toBeGreaterThanOrEqual(-0.5);
  expect(metrics.send!.bottom).toBeLessThanOrEqual(metrics.viewport + 0.5);
  expect(metrics.field, 'поле ввода есть в поле ввода').not.toBeNull();
  expect(metrics.field!.bottom).toBeLessThanOrEqual(metrics.viewport + 0.5);
}

/** Блок прокручивается внутри себя, а его последний элемент доезжает до вида. */
async function expectStackScrolls(page: Page, metrics: Metrics, stack: string, item: string, name: string): Promise<void> {
  expect(metrics.stack.top).toBeGreaterThanOrEqual(-0.5);
  expect(metrics.stack.bottom).toBeLessThanOrEqual(metrics.viewport + 0.5);
  expect(metrics.stack.bottom).toBeLessThanOrEqual(metrics.composer.top + 0.5);
  expect(metrics.overflowY, 'блок прокручивается сам').toBe('auto');
  expect(metrics.scrollHeight, 'содержимое блока выше его видимой части').toBeGreaterThan(metrics.clientHeight + 1);
  await page.locator(stack).evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  await page.waitForTimeout(80);
  const scrolled = await measure(page, stack, item);
  expect(scrolled.last, 'в блоке есть элементы').not.toBeNull();
  // Низ последнего элемента доезжает до нижней кромки: до конца блока можно прокрутить.
  expect(scrolled.last!.bottom).toBeLessThanOrEqual(scrolled.stack.bottom + 0.5);
  expect(scrolled.last!.bottom).toBeGreaterThan(scrolled.stack.top);
  // Карточка выше окна блока (ландшафт) видна не целиком — это допустимо, но верх короткой виден.
  if (scrolled.last!.height <= scrolled.stack.height + 0.5) {
    expect(scrolled.last!.top).toBeGreaterThanOrEqual(scrolled.stack.top - 0.5);
  }
  await shoot(page, `${name}-scrolled`);
}

function queueText(index: number, long: boolean): string {
  return long
    ? `Сообщение ${index + 1}: проверь гонку двух подписок при переподключении и собери короткий итог по каждому делегату`
    : `Сообщение ${index + 1}: собери итог`;
}

for (const viewport of VIEWPORTS) {
  const label = `${viewport.width}x${viewport.height}`;

  test.describe(`очередь запуска не выдавливает поле ввода ${label}`, () => {
    test('пять запусков в раскрытой очереди прокручиваются, поле ввода на месте', async ({ page }) => {
      await page.setViewportSize(viewport);
      const { hub, api, id } = await boot(page, (api) => seedMockupChat(api));
      for (let index = 0; index < 5; index++) {
        api.queueItem(id, {
          title: `Запуск ${index + 1}`,
          harness: 'claude',
          model: 'opus-5.5',
          reason: 'rate',
          position: index + 1,
          not_before: new Date(Date.now() + 10 * 60_000 * (index + 1)).toISOString()
        });
      }
      hub.flush();
      const strip = page.locator('[data-role="spawn-queue"]');
      await strip.locator('.head').click();
      await expect(strip.locator('li')).toHaveCount(5);
      // Раскрытие анимировано высотой: замер идёт после того, как список встал на место.
      await strip.locator('.fold').evaluate((el) => Promise.allSettled(el.getAnimations({ subtree: true }).map((item) => item.finished)));

      const metrics = await measure(page, '[data-role="spawn-queue"]', '[data-role="spawn-queue"] li');
      await shoot(page, 'spawn');
      expectComposerReachable(metrics);
      expect(metrics.feed.height).toBeGreaterThanOrEqual(FEED_FLOOR);
      await expectStackScrolls(page, metrics, '[data-role="spawn-queue"]', '[data-role="spawn-queue"] li', 'spawn');
    });
  });

  test.describe(`раскрытая очередь сообщений не выдавливает поле ввода ${label}`, () => {
    test('четырнадцать сообщений прокручиваются, «Свернуть» закреплена внизу', async ({ page }) => {
      await page.setViewportSize(viewport);
      const long = viewport.height <= 450;
      const { hub, api, id } = await boot(page, (api) => seedMockupChat(api));
      for (let index = 0; index < 14; index++) {
        hub.append(`conversation:${id}`, {
          kind: 'input_queued',
          conversation_id: id,
          payload: {
            input_id: api.id(),
            input_seq: String(index + 50),
            idempotency_key: api.id(),
            state: 'queued',
            content: [{ type: 'text', text: queueText(index, long) }]
          }
        });
      }
      hub.flush();
      const strip = page.locator('.strip');
      const more = strip.getByRole('button', { name: /^Ещё \d+ в очереди/ });
      await more.click();
      const fold = strip.getByRole('button', { name: 'Свернуть' });
      await expect(fold).toBeVisible();
      await expect(strip.locator('.item')).toHaveCount(14);

      const metrics = await measure(page, '.strip', '.strip > .item');
      await shoot(page, 'queue');
      expectComposerReachable(metrics);
      expect(metrics.feed.height).toBeGreaterThanOrEqual(FEED_FLOOR);
      await expectStackScrolls(page, metrics, '.strip', '.strip > .item', 'queue');

      // «Свернуть» стоит у нижней кромки блока и остаётся в окне после прокрутки.
      const after = await measure(page, '.strip', '.strip > .item');
      const foldBox = await fold.boundingBox();
      expect(foldBox).not.toBeNull();
      expect(foldBox!.y + foldBox!.height).toBeLessThanOrEqual(after.stack.bottom + 0.5);
      expect(foldBox!.y).toBeGreaterThanOrEqual(after.stack.top - 0.5);
      expect(foldBox!.y + foldBox!.height).toBeLessThanOrEqual(after.viewport + 0.5);
    });
  });

  test.describe(`карточки паузы не выдавливают поле ввода ${label}`, () => {
    test('две карточки прокручиваются в своём пределе, поле ввода на месте', async ({ page }) => {
      await page.setViewportSize(viewport);
      await boot(page, (api) => seedMockup08(api));
      await expect(page.locator('.pause-card')).toHaveCount(2);
      const metrics = await measure(page, '.pause-cards-feed', '.pause-card');
      await shoot(page, 'pause');
      expectComposerReachable(metrics);
      expect(metrics.feed.height).toBeGreaterThanOrEqual(FEED_FLOOR);
      await expectStackScrolls(page, metrics, '.pause-cards-feed', '.pause-card', 'pause');
      // Ввод доступен и после прокрутки карточек.
      await page.getByRole('textbox', { name: 'Сообщение' }).click();
      expect(await page.evaluate(() => document.activeElement?.tagName)).toBe('TEXTAREA');
    });
  });
}

test('колонка чата не растягивает документ: прокручивается только лента', async ({ page }) => {
  await page.setViewportSize({ width: 412, height: 450 });
  await boot(page, (api) => seedConversation(api, { title: 'Проверка колонки', model: 'opus-5.5' }));
  const overflow = await page.evaluate(() => ({
    height: document.documentElement.scrollHeight - document.documentElement.clientHeight,
    width: document.documentElement.scrollWidth - document.documentElement.clientWidth
  }));
  expect(overflow.height).toBeLessThanOrEqual(0);
  expect(overflow.width).toBeLessThanOrEqual(0);
});
