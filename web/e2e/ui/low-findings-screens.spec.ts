import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedTaigaCards } from '../../testing/fixtures/taiga-cards';
import { bootScreen, SCREENS, type Screen } from '../parity/states';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c1';

const screen = (name: string): Screen => SCREENS.find((item) => item.name === name)!;

const centerY = (box: { y: number; height: number }): number => box.y + box.height / 2;

async function settle(page: Page): Promise<void> {
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
}

// F-28: ответ в ветке без кнопок в подвале не должен держать пустую полосу 44 px.
test('F-28: lane answer card without footer actions has equal space above and below its text', async ({ page }) => {
  await bootScreen(page, screen('05-subagent-lane'));
  const cards = page.locator('article.card');
  await expect(cards.first()).toBeVisible();
  const quiet = cards.filter({ has: page.locator('.entry .text'), hasNot: page.locator('.footer-actions > *') });
  const count = await quiet.count();
  expect(count, 'lane has at least one answer card without footer actions').toBeGreaterThan(0);

  for (let index = 0; index < count; index++) {
    const card = quiet.nth(index);
    const box = (await card.boundingBox())!;
    const content = (await card.locator('.entry .text').first().boundingBox())!;
    const byline = (await card.locator('.byline p').boundingBox())!;
    const above = content.y - box.y;
    const below = box.y + box.height - (byline.y + byline.height);
    expect(Math.abs(above - below), `card ${index}: ${above} above, ${below} below`).toBeLessThanOrEqual(2);
  }
});

/** Лента виртуальная: докручиваем до карточки, пока она не смонтирована. */
async function reveal(page: Page, target: Locator): Promise<Locator> {
  const feed = page.locator('.feed');
  await feed.evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await settle(page);
  for (let step = 0; step < 60; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle(page);
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили.
      }
    }
    await feed.evaluate((node) => {
      node.scrollTop += node.clientHeight * 0.6;
    });
    await settle(page);
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

// F-29: карточка Taiga с одной шапкой равна шапке и держит текст по центру.
test('F-29: head-only taiga card is as tall as its head and centres its text', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedTaigaCards(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  const card = await reveal(page, page.locator('.feed-card[data-card="taiga.link"]').first());
  const box = (await card.boundingBox())!;
  const head = (await card.locator('.head').boundingBox())!;
  const lead = (await card.locator('.head .lead').boundingBox())!;
  expect(box.height - head.height, 'no empty body below the head').toBeLessThanOrEqual(0.5);
  expect(Math.abs(centerY(lead) - centerY(box)), 'head text is centred in the card').toBeLessThanOrEqual(1);
});
