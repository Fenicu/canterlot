import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { seedPresets } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

async function bootOwner(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  seedPresets(api);
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
  await page.goto('/presets');
  await expect(page.locator('[data-presets-ready]')).toBeVisible({ timeout: 15_000 });
  await page.locator('[data-preset="owner"]').getByRole('button', { name: 'Посмотреть' }).click();
  await expect(page.locator('[data-owner-summary]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

test('owner_sheet_shows_summary_and_renders_files_as_markdown', async ({ page }) => {
  await bootOwner(page);
  const summary = page.locator('[data-owner-summary]');
  await expect(summary.locator('li')).toHaveText([/^Claude Code: /, /^Codex: /, /^OpenCode: /, 'Память Claude Code: 1 проект']);
  await expect(page.locator('[data-owner-file="claude_user"]')).toContainText('/home/owner/.claude/CLAUDE.md');
  await expect(page.locator('[data-owner-file="claude_home"]')).toContainText('файл — ссылка, не показан');
  await page.locator('[data-owner-file="claude_user"] button').click();
  const content = page.locator('[data-owner-content="claude_user"]');
  await expect(content).toBeVisible();
  await expect(content.locator('h1, h2, p').first()).toBeVisible();
  await expect(content).toContainText('Стиль');
  await expect(page.locator('[data-owner-memory]')).toContainText('proj1');
});

test('owner_sheet_cards_stand_out_and_keep_meta_under_the_title', async ({ page }) => {
  await bootOwner(page);
  for (const theme of ['dark', 'light']) {
    if ((await page.locator('html').getAttribute('data-theme')) !== theme) await page.evaluate((value) => document.documentElement.setAttribute('data-theme', value), theme);
    const paint = await page.locator('[data-owner-summary]').evaluate((summary) => {
      const sheet = getComputedStyle(summary.closest('dialog')!).backgroundColor;
      const card = summary.parentElement!.querySelector('section')!;
      return { sheet, summary: getComputedStyle(summary).backgroundColor, card: getComputedStyle(card).backgroundColor };
    });
    // Итог и карточки файлов отличимы от листа, а не сливаются с ним или прозрачны.
    expect(paint.summary, theme).not.toBe(paint.sheet);
    expect(paint.summary, theme).not.toBe('rgba(0, 0, 0, 0)');
    expect(paint.card, theme).not.toBe(paint.sheet);
  }
  for (const kind of ['claude_user', 'codex_global', 'opencode_global']) {
    const card = page.locator(`[data-owner-file="${kind}"]`);
    const title = (await card.locator('.row strong').boundingBox())!;
    const meta = (await card.locator('.row .meta').boundingBox())!;
    // Подпись всегда под названием и с того же края, а не в строку и не справа.
    expect(meta.y, kind).toBeGreaterThanOrEqual(title.y + title.height - 1);
    expect(Math.abs(meta.x - title.x), kind).toBeLessThanOrEqual(1);
    await expect(card.locator('.meta')).not.toContainText(/\d\d\.\d\d\.\d{4}/);
  }
  const memory = page.locator('[data-owner-memory]');
  const select = (await memory.locator('select').boundingBox())!;
  const show = (await memory.getByRole('button', { name: 'Показать MEMORY.md' }).boundingBox())!;
  expect(show.y - (select.y + select.height)).toBeGreaterThanOrEqual(8);
});

test('owner_sheet_reads_memory_index_and_topic_file', async ({ page }) => {
  await bootOwner(page);
  await page.getByRole('button', { name: 'Показать MEMORY.md' }).click();
  await expect(page.locator('[data-owner-content="memory_index"]')).toContainText('тема');
  await page.locator('[data-owner-memory-file="a.md"]').click();
  await expect(page.locator('[data-owner-content="a.md"]')).toContainText('Тема');
});

test('owner_sheet_scrolls_long_files_and_supports_phone_desktop_and_themes', async ({ page }) => {
  for (const theme of ['dark', 'light'] as const) {
    for (const width of [384, 1280]) {
      await page.setViewportSize({ width, height: 800 });
      const hub = new FakeHub();
      const api = new FakeHubApi(hub);
      seedMockupSessions(api);
      seedPresets(api);
      await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
      await page.goto('/presets');
      await expect(page.locator('[data-presets-ready]')).toBeVisible({ timeout: 15_000 });
      if (theme === 'light') {
        const toggle = page.locator('main').getByRole('button', { name: 'Сменить тему' });
        if ((await page.locator('html').getAttribute('data-theme')) !== 'light') await toggle.click();
        await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
      }
      await page.locator('[data-preset="owner"]').getByRole('button', { name: 'Посмотреть' }).click();
      await expect(page.locator('[data-owner-summary]')).toBeVisible({ timeout: 15_000 });
      await page.locator('[data-owner-file="codex_global"] button').click();
      const box = page.locator('[data-owner-content="codex_global"]');
      await expect(box).toBeVisible();
      const scrollable = await box.evaluate((node) => node.scrollHeight <= node.clientHeight + 1 || getComputedStyle(node).overflowY === 'auto');
      expect(scrollable).toBe(true);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1)).toBe(true);
      await page.keyboard.press('Escape');
    }
  }
});

test('owner_sheet_is_read_only', async ({ page }) => {
  await bootOwner(page);
  await expect(page.locator('dialog textarea, dialog input[type="text"]')).toHaveCount(0);
});
