// Видео и снимки карточек Taiga для ревью: `PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/taiga-cards.spec.ts --project=phone`.
// Файлы ложатся в TAIGA_CARDS_OUT (по умолчанию /tmp/taiga-cards).
import { test, expect, type Browser, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdirSync, rmSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedTaigaCards, type TaigaCardsSeed } from '../../testing/fixtures/taiga-cards';

const OWNER = '00000000-0000-4000-8000-0000000000c1';
const OUT = process.env.TAIGA_CARDS_OUT ?? '/tmp/taiga-cards';

async function open(browser: Browser, opts: { width: number; height: number; theme: 'dark' | 'light'; video?: string }): Promise<{ page: Page; hub: FakeHub; api: FakeHubApi; seed: TaigaCardsSeed; close: () => Promise<void> }> {
  const context = await browser.newContext({
    viewport: { width: opts.width, height: opts.height },
    deviceScaleFactor: 2,
    colorScheme: opts.theme,
    ...(opts.video ? { recordVideo: { dir: opts.video, size: opts.width > 1000 ? { width: opts.width, height: opts.height } : { width: opts.width * 2, height: opts.height * 2 } } } : {})
  });
  await context.addInitScript((value) => localStorage.setItem('canterlot.theme', value), opts.theme);
  const page = await context.newPage();
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedTaigaCards(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed-card').first()).toBeVisible();
  return { page, hub, api, seed, close: () => context.close() };
}

/** Плавная прокрутка ленты: шаг за шагом, как пальцем. */
async function glide(page: Page, to: number | 'bottom', ms: number): Promise<void> {
  await page.locator('.feed').evaluate(
    (node, args) =>
      new Promise<void>((done) => {
        const from = node.scrollTop;
        const start = performance.now();
        const step = (time: number) => {
          const target = args.to === 'bottom' ? node.scrollHeight - node.clientHeight : args.to;
          const share = Math.min(1, (time - start) / args.ms);
          const eased = share < 0.5 ? 2 * share * share : 1 - (-2 * share + 2) ** 2 / 2;
          node.scrollTop = from + (target - from) * eased;
          if (share < 1) requestAnimationFrame(step);
          else done();
        };
        requestAnimationFrame(step);
      }),
    { to, ms }
  );
}

/** Карточка в виртуальной ленте: сверху вниз, пока строка не смонтируется. */
async function reveal(page: Page, selector: string): Promise<void> {
  await page.locator('.feed').evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  for (let step = 0; step < 80; step++) {
    const found = page.locator(selector).first();
    if ((await found.count()) > 0) {
      await found.scrollIntoViewIfNeeded();
      return;
    }
    await page.locator('.feed').evaluate((node) => {
      node.scrollTop += node.clientHeight * 0.6;
    });
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  }
  await expect(page.locator(selector).first()).toBeAttached();
}

function probe(mp4: string): { seconds: number; bytes: number } {
  const seconds = Number(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration', '-of', 'default=noprint_wrappers=1:nokey=1', mp4]).toString().trim());
  return { seconds, bytes: statSync(mp4).size };
}

test.skip(process.env.PW_SCREENSHOTS !== '1', 'снимки для ревью снимаются отдельно');

test('taiga_cards_videos_and_screenshots', async ({ browser }) => {
  test.setTimeout(600_000);
  mkdirSync(OUT, { recursive: true });
  const raw = resolve(OUT, 'raw-video');
  rmSync(raw, { recursive: true, force: true });

  await record(browser, { width: 390, height: 844, theme: 'dark', name: 'taiga-cards-phone-dark', scale: 780, raw });
  await record(browser, { width: 390, height: 844, theme: 'light', name: 'taiga-cards-phone-light', scale: 780, raw });
  await record(browser, { width: 1440, height: 900, theme: 'dark', name: 'taiga-cards-desktop-dark', scale: 1440, raw });
  rmSync(raw, { recursive: true, force: true });
});

/** Сценарий видео: весь ход по Taiga, раскрытый комментарий, правка, списки, идущий вызов и ошибка. */
async function record(browser: Browser, opts: { width: number; height: number; theme: 'dark' | 'light'; name: string; scale: number; raw: string }): Promise<void> {
  const { page, close } = await open(browser, { width: opts.width, height: opts.height, theme: opts.theme, video: opts.raw });
  const video = page.video();
  await page.locator('.feed').evaluate((node) => { node.scrollTop = 0; });
  await page.waitForTimeout(1500);
  // Лента сверху вниз: поиск, история, служебные строки, комментарии, правка, задачи, связь, списки, вики, удаление.
  await glide(page, 'bottom', 20_000);
  await page.waitForTimeout(900);

  // Длинный комментарий раскрывается целиком.
  await reveal(page, '.feed-card[data-card="taiga.comment_add"]');
  await page.waitForTimeout(900);
  await page.locator('.feed-card[data-card="taiga.comment_add"] [data-role="taiga-comment-toggle"]').click();
  await page.waitForTimeout(2400);

  // Правка истории — самое важное место: что именно изменилось.
  await reveal(page, '.feed-card[data-card="taiga.userstory_update"]');
  await page.waitForTimeout(1200);
  await page.screenshot({ path: resolve(OUT, `${opts.name}.png`) });
  await page.waitForTimeout(2200);

  // Список спринта раскрывается до конца.
  await reveal(page, '.feed-card[data-card="taiga.userstory_list"]');
  await page.waitForTimeout(800);
  await page.locator('.feed-card[data-card="taiga.userstory_list"] [data-role="taiga-more"]').click();
  await page.waitForTimeout(2400);

  // Пачка созданных задач тоже раскрывается.
  await reveal(page, '.feed-card[data-card="taiga.bulk_create"]');
  await page.waitForTimeout(800);
  await page.locator('.feed-card[data-card="taiga.bulk_create"] [data-role="taiga-more"]').click();
  await page.waitForTimeout(2200);

  // Идущий вызов и ошибка: состояние словом.
  await reveal(page, `.feed-card[data-card="taiga.userstory_get"][data-state="running"]`);
  await page.waitForTimeout(1800);
  await reveal(page, `.feed-card[data-card="taiga.userstory_update"][data-state="error"]`);
  await page.waitForTimeout(2200);
  await glide(page, 'bottom', 3500);
  await page.waitForTimeout(1200);
  await close();

  const webm = await video!.path();
  const mp4 = resolve(OUT, `${opts.name}.mp4`);
  execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-i', webm, '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', '22', '-movflags', '+faststart', '-vf', `scale=${opts.scale}:-2`, mp4]);
  rmSync(webm, { force: true });
  const { seconds, bytes } = probe(mp4);
  expect(seconds, `${opts.name}: ${seconds} s`).toBeGreaterThanOrEqual(20);
  expect(seconds, `${opts.name}: ${seconds} s`).toBeLessThanOrEqual(60);
  expect(bytes, `${opts.name}: ${Math.round(bytes / 1_000_000)} MB`).toBeLessThan(40_000_000);
}
