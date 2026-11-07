// Видео и снимки карточек ленты для ревью: `PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/feed-cards.spec.ts --project=phone`.
// Файлы ложатся в FEED_CARDS_OUT (по умолчанию /tmp/feed-cards).
import { test, expect, type Browser, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { TODO_SECOND, hubText, seedFeedCards, setActivity, streamCall, type FeedCardsSeed } from '../../testing/fixtures/feed-cards';

const OWNER = '00000000-0000-4000-8000-0000000000c1';
const OUT = process.env.FEED_CARDS_OUT ?? '/tmp/feed-cards';
const IMAGE = readFileSync(resolve(import.meta.dirname, '../../../docs/screenshots/chat-dark.png'));

async function open(browser: Browser, opts: { width: number; height: number; theme: 'dark' | 'light'; video?: string }): Promise<{ page: Page; hub: FakeHub; api: FakeHubApi; seed: FeedCardsSeed; close: () => Promise<void> }> {
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
  const seed = seedFeedCards(api);
  api.details.get(seed.id)!.activity = { state: 'working', evidence: {}, since: new Date().toISOString() };
  await installChatRoutes(page, hub, api, OWNER);
  await page.route(`**/api/v1/attachments/${seed.imageId}/content`, (route) => route.fulfill({ contentType: 'image/png', body: IMAGE }));
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

async function scrollTo(page: Page, selector: string, ms = 1600): Promise<void> {
  const top = await page.locator('.feed').evaluate((node, sel) => {
    const target = node.querySelector(sel) as HTMLElement | null;
    if (!target) return node.scrollTop;
    return node.scrollTop + target.getBoundingClientRect().top - node.getBoundingClientRect().top - 120;
  }, selector);
  await glide(page, top, ms);
}

test.skip(process.env.PW_SCREENSHOTS !== '1', 'снимки для ревью снимаются отдельно');

test('feed_cards_video_and_screenshots', async ({ browser }) => {
  test.setTimeout(420_000);
  mkdirSync(OUT, { recursive: true });
  const raw = resolve(OUT, 'raw-video');
  rmSync(raw, { recursive: true, force: true });

  // Снимки: телефон тёмный, стол светлый.
  {
    const { page, close } = await open(browser, { width: 390, height: 844, theme: 'dark' });
    await page.locator('.feed').evaluate((node) => { node.scrollTop = 0; });
    await scrollTo(page, '.feed-card[data-card="canterlot.limits"]', 10);
    await page.waitForTimeout(600);
    await page.screenshot({ path: resolve(OUT, 'phone-390-dark.png') });
    await close();
  }
  {
    const { page, close } = await open(browser, { width: 1440, height: 900, theme: 'light' });
    await page.locator('.feed').evaluate((node) => { node.scrollTop = 0; });
    await scrollTo(page, '.feed-card[data-card="canterlot.limits"]', 10);
    await page.waitForTimeout(600);
    await page.screenshot({ path: resolve(OUT, 'desktop-1440-light.png') });
    await close();
  }

  // Видео: телефон 390×844 тёмный и рабочий стол 1440×900 светлый, один сценарий.
  await record(browser, { width: 390, height: 844, theme: 'dark', name: 'feed-cards-phone-dark', scale: 780, raw });
  await record(browser, { width: 1440, height: 900, theme: 'light', name: 'feed-cards-desktop-light', scale: 1440, raw });
  rmSync(raw, { recursive: true, force: true });
});

/** Сценарий видео: лента целиком, живой запуск делегата, обновление плана, просмотр файла и сырой вызов. */
async function record(browser: Browser, opts: { width: number; height: number; theme: 'dark' | 'light'; name: string; scale: number; raw: string }): Promise<void> {
  const { page, hub, api, seed, close } = await open(browser, { width: opts.width, height: opts.height, theme: opts.theme, video: opts.raw });
  const video = page.video();
  await page.locator('.feed').evaluate((node) => { node.scrollTop = 0; });
  await page.waitForTimeout(1500);
  // Лента сверху вниз: задачи, служебные вызовы, лимиты, запуски, список, будильники, веб, Taiga.
  await glide(page, 'bottom', 22_000);
  await page.waitForTimeout(800);

  // Новый делегат на глазах: карточка вырастает, «запускается» → живое состояние ребёнка.
  setActivity(hub, api, seed.second, 'working', { phase: 'running_tool' });
  const fresh = streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__session_spawn', { title: 'Ревью вёрстки', brief: 'Проверь карточки на 320 px.\nСравни с макетом.', harness: 'codex', model: 'gpt-6.1-sol', effort: 'high', isolated: true });
  await page.waitForTimeout(1800);
  streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__session_spawn', null, { blockId: fresh.call, resultId: fresh.result, result: hubText({ permission_mode: 'bypass', session_id: seed.second, status: 'started', subscribed: true, worktree: { branch: 'canterlot/0a1b2c3d', path: '/w' } }) });
  await page.waitForTimeout(2200);
  setActivity(hub, api, seed.second, 'waiting_for_user', { question: 'Какой брейкпоинт?' });
  await page.waitForTimeout(1800);
  setActivity(hub, api, seed.second, 'idle');
  await page.waitForTimeout(1800);

  // План обновился: закрытый пункт зачёркивается на глазах.
  const next = { todos: TODO_SECOND.todos.map((item) => (item.content === 'Карточка запуска делегата' ? { ...item, status: 'completed' } : item.content === 'Тесты и видео' ? { ...item, status: 'in_progress' } : item)) };
  streamCall(hub, api, seed.id, seed.run, 'TodoWrite', next, { result: 'Todos have been modified successfully.' });
  await page.waitForTimeout(3000);

  // Файл для владельца: миниатюра открывает просмотрщик.
  await scrollTo(page, '.feed-card[data-card="canterlot.share_file"]', 1800);
  await page.waitForTimeout(800);
  await page.locator('.feed-card[data-card="canterlot.share_file"] button.thumb').click();
  await expect(page.locator('dialog[open]')).toBeVisible();
  await page.waitForTimeout(2500);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(900);

  // Сырой вызов раскрывается из любой карточки.
  await scrollTo(page, '.feed-card[data-card="canterlot.limits"]', 2200);
  await page.waitForTimeout(500);
  await page.locator('.feed-card[data-card="canterlot.limits"] .hit').click();
  await page.waitForTimeout(2600);
  await page.locator('.feed-card[data-card="canterlot.limits"] .hit').click();
  await page.waitForTimeout(600);
  await scrollTo(page, `[data-key="block:${seed.blocks.spawn}"]`, 1500);
  await page.locator(`[data-key="block:${seed.blocks.spawn}"] [data-role="spawn-brief"]`).click();
  await page.waitForTimeout(2400);
  await glide(page, 'bottom', 3000);
  await page.waitForTimeout(1200);
  await close();

  const webm = await video!.path();
  const mp4 = resolve(OUT, `${opts.name}.mp4`);
  execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-i', webm, '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', '22', '-movflags', '+faststart', '-vf', `scale=${opts.scale}:-2`, mp4]);
  rmSync(webm, { force: true });
}
