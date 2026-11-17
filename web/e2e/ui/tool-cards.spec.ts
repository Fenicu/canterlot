import { mkdirSync } from 'node:fs';
import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { streamCall } from '../../testing/fixtures/feed-cards';
import { LONG_COMMAND, seedToolCards, type ToolCardsSeed } from '../../testing/fixtures/tool-cards';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c1';
const SHOTS = '/tmp/bubbles-shots';

async function boot(page: Page, opts: { working?: boolean } = {}): Promise<{ hub: FakeHub; api: FakeHubApi; seed: ToolCardsSeed }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedToolCards(api, opts);
  if (opts.working) api.details.get(seed.id)!.activity = { state: 'working', evidence: {}, since: new Date().toISOString() };
  // Буфер обмена подменяется: проверка смотрит, что именно ушло в него, в любом браузере.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async (text: string) => { (window as unknown as { __copied?: string }).__copied = text; } } });
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, seed };
}

const feed = (page: Page) => page.locator('.feed');
const row = (page: Page, id: string) => page.locator(`[data-key="block:${id}"]`);

/** Лента виртуальная: строка далеко от низа не смонтирована, пока до неё не докрутили. */
async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await feed(page).evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await settle();
  for (let step = 0; step < 60; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle();
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили: ищем дальше.
      }
    }
    await feed(page).evaluate((node) => {
      node.scrollTop += node.clientHeight * 0.6;
    });
    await settle();
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

async function cardOf(page: Page, id: string): Promise<Locator> {
  return reveal(page, row(page, id).locator('.feed-card'));
}

async function shot(page: Page, target: Locator, name: string, opts: { moving?: boolean } = {}): Promise<void> {
  if (test.info().project.name !== 'phone') return;
  mkdirSync(SHOTS, { recursive: true });
  await page.mouse.move(0, 0);
  await target.screenshot({ path: `${SHOTS}/${name}.png`, animations: opts.moving ? 'allow' : 'disabled' });
}

test.describe('shell card', () => {
  test('shell_card_shows_the_formatted_highlighted_command_and_copies_the_original', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.shellLong!);
    await expect(card).toHaveAttribute('data-card', 'shell.exec');
    await expect(card.locator('[data-role="shell-title"]')).toHaveText('Проверки веба: токены и юнит-тесты');
    const rows = card.locator('pre.code .row');
    await expect(rows).toHaveText(['cd web', '  && npm ci --no-audit --no-fund', '  && npm run lint:tokens', '  && npm run test:unit -- --reporter=dot 2>&1', '    | tail -20']);
    // Подсветка приходит ленивым модулем highlight.js: имена команд красятся токеном.
    await expect(card.locator('pre.code [class*="hljs-"]').first()).toBeAttached();
    await card.getByRole('button', { name: 'Скопировать команду' }).click();
    await expect(card.locator('.status[role="status"]')).toHaveText('Скопировано');
    expect(await page.evaluate(() => (window as unknown as { __copied?: string }).__copied)).toBe(LONG_COMMAND);
    await shot(page, card, 'shell-phone');
  });

  test('shell_output_is_collapsed_and_expands_to_its_tail', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.shellLong!);
    await expect(card.locator('[data-role="card-fold"]')).toHaveCount(0);
    await expect(card.locator('[data-role="shell-meta"]')).toContainText('вывод · 34 строки');
    const head = card.locator('.head .hit');
    await expect(head).toHaveAttribute('aria-label', 'Проверки веба: токены и юнит-тесты. Показать вывод');
    await head.click();
    const fold = card.locator('[data-role="card-fold"]');
    await expect(fold.locator('pre')).toContainText('Duration  4.21s');
    await expect(fold.locator('pre')).not.toContainText('case-1.test.ts');
    await expect(fold.getByRole('button', { name: /Показать полностью/ })).toBeVisible();
    await shot(page, card, 'shell-expanded-phone');
    await head.click();
    await expect(fold).toHaveCount(0);
  });

  test('exit_codes_denials_and_cwd_read_in_their_tones', async ({ page }) => {
    const { seed } = await boot(page);
    const failed = await cardOf(page, seed.blocks.shellFailed!);
    await expect(failed.locator('[data-role="card-state"]')).toHaveText('код 1');
    await expect(failed.locator('[data-role="card-state"]')).toHaveAttribute('data-tone', 'danger');
    await expect(failed.locator('[data-role="shell-title"]')).toHaveText('Команда');
    await shot(page, failed, 'shell-failed-phone');
    const pipeline = await cardOf(page, seed.blocks.shellPipeline!);
    await expect(pipeline.locator('[data-role="card-state"]')).toHaveCount(0);
    await expect(pipeline.locator('[data-role="shell-meta"]')).toContainText('код 0');
    await expect(pipeline.locator('pre.code .row')).toHaveCount(6);
    const denied = await cardOf(page, seed.blocks.shellDenied!);
    await expect(denied.locator('[data-role="card-state"]')).toHaveText('отклонено');
    await expect(denied.locator('[data-role="card-state"]')).toHaveAttribute('data-tone', 'warning');
    const cwd = await cardOf(page, seed.blocks.shellCwd!);
    await expect(cwd.locator('[data-role="shell-cwd"]')).toHaveText('/home/owner/canterlot');
  });

  test('a_running_command_reads_as_running_and_settles_with_its_output', async ({ page }) => {
    const { hub, api, seed } = await boot(page, { working: true });
    const fresh = streamCall(hub, api, seed.id, seed.run, 'Bash', { command: 'npm run build', description: 'Сборка веба' });
    const card = row(page, fresh.call).locator('.feed-card');
    await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
    await expect(card).toHaveAttribute('data-state', 'running');
    streamCall(hub, api, seed.id, seed.run, 'Bash', null, { blockId: fresh.call, resultId: fresh.result, result: 'built in 3.1s' });
    await expect(card).toHaveAttribute('data-state', 'done');
    await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
  });

  test('a_long_command_scrolls_inside_its_block_not_the_page_at_360px', async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 760 });
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.shellLong!);
    const code = card.locator('pre.code');
    const sizes = await code.evaluate((node) => ({ scroll: node.scrollWidth, client: node.clientWidth }));
    expect(sizes.scroll).toBeGreaterThan(sizes.client);
    const page360 = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
    expect(page360.scroll).toBeLessThanOrEqual(page360.client);
    const feedWidth = await feed(page).evaluate((node) => node.scrollWidth - node.clientWidth);
    expect(feedWidth).toBeLessThanOrEqual(0);
  });
});

/** Имена анимаций на блике навыка: пусто — имя стоит спокойно. */
async function glints(card: Locator): Promise<string[]> {
  return card.locator('.glint').evaluate((node) => node.getAnimations().map((item) => (item as CSSAnimation).animationName));
}

test.describe('skill card', () => {
  test('a_finished_skill_shows_its_name_plugin_and_args_and_stands_still', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.skillDone!);
    await expect(card).toHaveAttribute('data-card', 'skill');
    await expect(card.locator('[data-role="skill-name"]')).toContainText('test-driven-development');
    await expect(card.locator('[data-role="skill-plugin"]')).toHaveText('superpowers');
    await expect(card.locator('[data-role="skill-args"]')).toHaveText('карточки ленты: формат команды и счёт строк');
    await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
    expect(await glints(card)).toEqual([]);
    const opencode = await cardOf(page, seed.blocks.skillOpencode!);
    await expect(opencode.locator('[data-role="skill-name"]')).toContainText('frontend-design');
    await shot(page, card, 'skill-phone');
  });

  for (const motion of ['full', 'reduced'] as const) {
    test(`a_running_skill_glints_until_it_loads_${motion}`, async ({ page }) => {
      if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
      const { hub, api, seed } = await boot(page, { working: true });
      const fresh = streamCall(hub, api, seed.id, seed.run, 'Skill', { skill: 'antislop:antislop-ui' });
      const card = row(page, fresh.call).locator('.feed-card');
      await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
      await expect(card.locator('[data-role="skill-name"]')).toHaveAttribute('data-glint', '');
      if (motion === 'full') {
        await expect.poll(async () => (await glints(card)).map((name) => name.replace(/^svelte-\w+-/, ''))).toEqual(['skill-glint']);
        // Снимок посреди прохода блика: полоса акцента на имени.
        await page.waitForTimeout(700);
        await shot(page, card, 'skill-running-phone', { moving: true });
      } else {
        expect(await glints(card)).toEqual([]);
      }
      streamCall(hub, api, seed.id, seed.run, 'Skill', null, { blockId: fresh.call, resultId: fresh.result, result: 'Launching skill: antislop:antislop-ui' });
      await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
      await expect(card.locator('[data-role="skill-name"]')).not.toHaveAttribute('data-glint');
      expect(await glints(card)).toEqual([]);
    });
  }
});
