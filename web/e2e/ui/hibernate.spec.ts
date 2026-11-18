import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const HIBERNATED = { state: 'stopped', evidence: { outcome: 'hibernated' }, since: '2026-10-28T19:00:00.000Z' } as never;

const CATALOG = {
  id: 'workstation',
  name: 'workstation',
  harnesses: [
    {
      id: 'claude',
      title: 'Claude Code',
      provider: 'claude',
      models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
      permission_modes: ['owner', 'ask', 'bypass', 'read_only']
    }
  ]
};

async function bootChat(page: Page, hibernated: boolean) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, CATALOG as never);
  const id = seedConversation(api, { title: 'Усыплённая сессия', model: 'opus-5.5' });
  if (hibernated) {
    api.details.get(id)!.activity = HIBERNATED;
    (api.list.get(id) as { activity?: unknown }).activity = HIBERNATED;
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { api, id };
}

test('sessions_list_shows_hibernated_instead_of_stopped', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, CATALOG as never);
  seedConversation(api, { title: 'Усыплённая сессия', model: 'opus-5.5' });
  const id = [...api.list.keys()][0]!;
  api.details.get(id)!.activity = HIBERNATED;
  (api.list.get(id) as { activity?: unknown }).activity = HIBERNATED;
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const row = page.locator('.row[data-state="hibernated"]');
  await expect(row).toBeVisible();
  await expect(row).toContainText('Усыплена');
  await expect(page.locator('.row[data-state="stopped"]')).toHaveCount(0);
  await expect(row.locator('.dot')).toHaveAttribute('data-token', '--muted');
});

test('chat_header_shows_hibernated_without_a_live_process_dot', async ({ page }) => {
  await bootChat(page, true);
  const mark = page.locator('.mark[data-state="hibernated"]');
  await expect(mark).toBeAttached();
  await expect(mark).toHaveAttribute('title', 'Усыплена');
  await expect(page.locator('.state-chip')).toContainText('Усыплена');
  await expect(page.locator('.mark[data-state="stopped"]')).toHaveCount(0);
  await expect(mark).toHaveClass(/dim/);
});

test('hibernation_setting_off_is_saved_on_the_hub', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/settings?c=limits');
  const section = page.locator('[data-hibernate-settings]');
  await expect(section.getByRole('heading', { name: 'Усыпление простаивающих сессий' })).toBeVisible({ timeout: 15_000 });
  const select = section.getByLabel('Усыплять после простоя');
  await expect(select).toHaveValue('60');
  await select.selectOption('off');
  await expect.poll(() => api.hibernateIdleMinutes).toBe(null);
  await select.selectOption('120');
  await expect.poll(() => api.hibernateIdleMinutes).toBe(120);
});
