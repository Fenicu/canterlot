// Общее для спеков карточек задач: лента виртуальная, строку далеко от низа надо докрутить.
import type { Locator, Page } from '@playwright/test';
import { expect } from './fixtures';

export const feed = (page: Page) => page.locator('.feed');

/** Лента виртуальная: строка далеко от низа не смонтирована, пока до неё не докрутили. */
export async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await feed(page).evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await settle();
  for (let step = 0; step < 40; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle();
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили: ищем дальше.
      }
    }
    await feed(page).evaluate((node) => { node.scrollTop += node.clientHeight * 0.6; });
    await settle();
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

export const cardOf = (page: Page, blockId: string) => page.locator(`[data-key="block:${blockId}"] .feed-card`).first();
