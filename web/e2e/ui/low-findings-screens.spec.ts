import AxeBuilder from '@axe-core/playwright';
import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedTaigaCards } from '../../testing/fixtures/taiga-cards';
import { bootScreen, SCREENS, type Screen } from '../parity/states';
import { bootSecondary } from './secondary-fixture';
import { viewer } from './image-viewer-fixture';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c1';

const screen = (name: string): Screen => SCREENS.find((item) => item.name === name)!;

const centerY = (box: { y: number; height: number }): number => box.y + box.height / 2;

/** Ряды, в которые раскладывается элемент: сколько разных верхних краёв у его детей. */
async function rows(locator: Locator): Promise<number> {
  const tops = await locator.evaluateAll((nodes) => nodes.map((node) => Math.round(node.getBoundingClientRect().top)));
  return new Set(tops).size;
}

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

async function downloadRow(page: Page, name: string): Promise<void> {
  await bootScreen(page, screen(name));
  const dialog = page.getByRole('dialog').first();
  await expect(dialog).toBeVisible();
  const close = dialog.getByRole('button', { name: 'Закрыть' });
  const download = dialog.getByRole('link', { name: /^Скачать/ });
  const header = (await dialog.locator('header').boundingBox())!;
  const closeBox = (await close.boundingBox())!;
  const downloadBox = (await download.boundingBox())!;
  // Кнопка скачивания живёт в шапке рядом с «Закрыть», а не отдельной строкой над документом.
  expect(Math.abs(centerY(downloadBox) - centerY(closeBox)), `${name}: download and close share a row`).toBeLessThanOrEqual(4);
  expect(downloadBox.y, `${name}: download lies inside the header`).toBeGreaterThanOrEqual(header.y - 2);
  expect(downloadBox.y + downloadBox.height, `${name}: download lies inside the header`).toBeLessThanOrEqual(header.y + header.height + 2);
}

test('F-31: word viewer keeps the download beside the close button', async ({ page }) => {
  await downloadRow(page, 'docx-viewer');
});

test('F-31: excel viewer keeps the download beside the close button', async ({ page }) => {
  await downloadRow(page, 'xlsx-viewer');
});

// F-32: подпись просмотрщика не повторяет заголовок карточки.
test('F-32: image viewer caption prints the title once', async ({ page }) => {
  await bootScreen(page, screen('image-viewer'));
  const caption = viewer(page).locator('.caption');
  await expect(caption).toBeVisible();
  const text = (await caption.textContent()) ?? '';
  expect(text.match(/Рабочий стол 1/g) ?? [], `caption: ${text}`).toHaveLength(1);
});

// F-34: причина стоит рядом со своей кнопкой и по центру её высоты, а не отдельной строкой.
test('F-34: routing action reasons are centred against their buttons', async ({ page }) => {
  await bootSecondary(page, '/settings/routing');
  const actions = page.locator('[data-role="policy-actions"]');
  for (const width of [360, 412]) {
    await page.setViewportSize({ width, height: 892 });
    await settle(page);
    await actions.scrollIntoViewIfNeeded();
    const reasons = actions.locator('.reason');
    const count = await reasons.count();
    expect(count, `${width}: disabled actions carry a reason`).toBeGreaterThan(0);
    for (let index = 0; index < count; index++) {
      const reason = reasons.nth(index);
      const button = reason.locator('xpath=preceding-sibling::button[1]');
      await expect(button).toHaveCount(1);
      const reasonBox = (await reason.boundingBox())!;
      const buttonBox = (await button.boundingBox())!;
      expect(
        Math.abs(centerY(reasonBox) - centerY(buttonBox)),
        `${width}: reason «${(await reason.textContent())?.trim()}» is centred against its button`
      ).toBeLessThanOrEqual(2);
    }
  }
});

// F-36: пять коротких вариантов срока бездействия помещаются в одну строку и на 360.
test('F-36: idle timeout segmented control stays on one row at 360', async ({ page }) => {
  await bootSecondary(page, '/settings/sign-in');
  await page.setViewportSize({ width: 360, height: 892 });
  const idle = page.getByRole('radiogroup', { name: 'Срок бездействия' });
  await expect(idle).toBeVisible();
  const labels = idle.locator('label');
  await expect(labels).toHaveCount(5);
  expect(await rows(labels), 'five options share one row').toBe(1);
  const box = (await idle.boundingBox())!;
  expect(box.height, 'control is one 44 px row plus padding').toBeLessThanOrEqual(60);
});

test('F-36: other segmented controls keep their rows at 360', async ({ page }) => {
  await bootSecondary(page, '/settings?c=appearance');
  await page.setViewportSize({ width: 360, height: 892 });
  for (const name of ['Режим', 'Скорость анимаций']) {
    const group = page.getByRole('radiogroup', { name });
    await expect(group).toBeVisible();
    expect(await rows(group.locator('label')), `${name} stays on one row`).toBe(1);
  }
});

/** Продукт держит html нулевой высоты: на время axe поднимаем корень, как parity-проверка. */
async function liftRootClipping(page: Page): Promise<void> {
  await page.evaluate(() => {
    document.documentElement.style.setProperty('overflow', 'visible', 'important');
    document.documentElement.style.setProperty('min-height', '100dvh');
    document.body.style.setProperty('min-height', '100dvh');
    document.getElementById('app-shell')?.removeAttribute('inert');
  });
}

async function linkInTextViolations(page: Page): Promise<string[]> {
  await liftRootClipping(page);
  const results = await new AxeBuilder({ page }).withRules(['link-in-text-block']).analyze();
  return results.violations.flatMap((violation) =>
    violation.nodes.slice(0, 5).map((node) => `${node.target.join(' ')} — ${node.any[0]?.message ?? violation.help}`)
  );
}

// X-2: ссылка плана внутри строки карточки отличается не только цветом.
test('X-2: plan link inside a card line is distinguishable without colour', async ({ page }) => {
  await bootScreen(page, screen('plan-cards'));
  expect(await linkInTextViolations(page)).toEqual([]);
});

test('X-2: plan tab keeps link-in-text-block clean', async ({ page }) => {
  await bootScreen(page, screen('plan-tab'));
  expect(await linkInTextViolations(page)).toEqual([]);
});
