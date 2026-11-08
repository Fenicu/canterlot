// Снимки карточек доски плана для ревью: `PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/plan-cards.spec.ts --project=phone`.
// Файлы ложатся в PLAN_CARDS_OUT (по умолчанию /tmp/plan-cards), список — в index.txt.
import { test, expect, type Browser, type Locator, type Page } from '@playwright/test';
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedPlanCards, type PlanCardsSeed } from '../../testing/fixtures/plan-cards';
import { PALETTES } from '../../src/lib/palettes';

const OWNER = '00000000-0000-4000-8000-0000000000c2';
const OUT = process.env.PLAN_CARDS_OUT ?? '/tmp/plan-cards';

interface Shot {
  name: string;
  /** Как раскрыть карточку: кнопка внутри неё или сырой вызов по шеврону. */
  expand: string | 'raw';
  what: string;
}

const SHOTS: Shot[] = [
  { name: 'add', expand: 'Все задачи · 5', what: 'plan_add: «План: добавлено 5 задач», номера и названия; раскрыто — весь список' },
  { name: 'addError', expand: 'raw', what: 'plan_add, invalid_arguments: русская строка ошибки; раскрыто — сырой вызов' },
  { name: 'addFull', expand: 'raw', what: 'plan_add, plan_full' },
  { name: 'spawnPlan', expand: 'raw', what: 'session_spawn с plan_task: строка «задача #5 …» с живым состоянием' },
  { name: 'spawnPlanError', expand: 'raw', what: 'session_spawn, plan_task_closed' },
  { name: 'updateStart', expand: 'raw', what: 'plan_update: «ждёт → в работе», справа «сейчас готова» по живому плану' },
  { name: 'updateBlocked', expand: 'raw', what: 'plan_update: переход в «заблокирована» и блокер' },
  { name: 'updateReview', expand: 'raw', what: 'plan_update: «на проверке» и находки' },
  { name: 'updateError', expand: 'raw', what: 'plan_update, plan_task_not_found' },
  { name: 'list', expand: 'Весь план · 5', what: 'plan_list: сводка состояний; раскрыто — задачи в порядке экрана плана' },
  { name: 'listError', expand: 'raw', what: 'plan_list, invalid_arguments' },
  { name: 'adopt', expand: 'raw', what: 'plan_adopt: «Забрала план «…»: 3 задачи» и соответствие номеров' },
  { name: 'adoptError', expand: 'raw', what: 'plan_adopt, invalid_arguments' }
];

async function open(browser: Browser, width: number, height: number, theme: 'dark' | 'light', palette = 'standard'): Promise<{ page: Page; seed: PlanCardsSeed; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: 2, colorScheme: theme, reducedMotion: 'reduce' });
  await context.addInitScript(([value, pal]) => {
    localStorage.setItem('canterlot.theme', value!);
    localStorage.setItem('canterlot.palette', pal!);
  }, [theme, palette]);
  const page = await context.newPage();
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedPlanCards(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed-card').first()).toBeVisible();
  return { page, seed, close: () => context.close() };
}

async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await page.locator('.feed').evaluate((node) => { node.scrollTop = 0; });
  await settle();
  for (let step = 0; step < 60; step++) {
    if ((await target.count()) > 0) {
      await target.first().scrollIntoViewIfNeeded();
      await settle();
      return target.first();
    }
    await page.locator('.feed').evaluate((node) => { node.scrollTop += node.clientHeight * 0.6; });
    await settle();
  }
  throw new Error('card not found');
}

async function shoot(browser: Browser, label: string, width: number, height: number, theme: 'dark' | 'light'): Promise<void> {
  const { page, seed, close } = await open(browser, width, height, theme);
  for (const shot of SHOTS) {
    const card = await reveal(page, page.locator(`[data-key="block:${seed.blocks[shot.name]}"] .feed-card`));
    const base = `${label}-${shot.name}`;
    await card.screenshot({ path: `${OUT}/${base}-collapsed.png` });
    if (shot.expand === 'raw') await card.locator('.hit').dispatchEvent('click');
    else await card.getByRole('button', { name: shot.expand }).click();
    await page.waitForTimeout(250);
    await card.scrollIntoViewIfNeeded();
    await card.screenshot({ path: `${OUT}/${base}-expanded.png` });
    appendFileSync(`${OUT}/index.txt`, `${base}-collapsed.png / ${base}-expanded.png — ${shot.what}\n`);
  }
  await page.screenshot({ path: `${OUT}/${label}-feed.png` });
  appendFileSync(`${OUT}/index.txt`, `${label}-feed.png — лента целиком\n`);
  await close();
}

test('plan_cards_screenshots', async ({ browser }) => {
  test.setTimeout(240_000);
  mkdirSync(OUT, { recursive: true });
  writeFileSync(`${OUT}/index.txt`, 'Карточки доски плана в ленте: телефон 390x844 тёмная, стол 1280x800 светлая.\n');
  await shoot(browser, 'phone-dark', 390, 844, 'dark');
  await shoot(browser, 'desktop-light', 1280, 800, 'light');
  // Палитры: карточка правки и раскрытый список в каждой, обе темы.
  for (const theme of ['dark', 'light'] as const) {
    for (const palette of PALETTES) {
      const { page, seed, close } = await open(browser, 390, 844, theme, palette.key);
      const list = await reveal(page, page.locator(`[data-key="block:${seed.blocks.list}"] .feed-card`));
      await list.getByRole('button', { name: 'Весь план · 5' }).click();
      await list.scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${OUT}/palette-${theme}-${palette.key}.png` });
      await close();
    }
  }
  appendFileSync(`${OUT}/index.txt`, 'palette-<тема>-<палитра>.png — раскрытая карточка plan_list и правки рядом во всех 15 палитрах\n');
});
