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
  await expect(page.locator('[data-owner-summary]')).toContainText('claude:');
  await expect(page.locator('[data-owner-summary]')).toContainText('codex:');
  await expect(page.locator('[data-owner-summary]')).toContainText('opencode:');
  await expect(page.locator('[data-owner-file="claude_user"]')).toContainText('/home/owner/.claude/CLAUDE.md');
  await expect(page.locator('[data-owner-file="claude_home"]')).toContainText('отсутствует');
  await page.locator('[data-owner-file="claude_user"] button').click();
  const content = page.locator('[data-owner-content="claude_user"]');
  await expect(content).toBeVisible();
  await expect(content.locator('h1, h2, p').first()).toBeVisible();
  await expect(content).toContainText('Стиль');
  await expect(page.locator('[data-owner-memory]')).toContainText('proj1');
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
