import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, resizeViewport, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

test('at_360px_the_status_is_whole_and_the_model_machine_suffix_is_what_truncates', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const workstation = seedMachine(api, { id: api.id(), name: 'workstation', availability: 'ready' });
  seedMachine(api, { id: api.id(), name: 'mini', availability: 'ready' });
  const at = new Date(Date.now() - 12 * 60_000).toISOString();
  const id = seedConversation(api, {
    title: 'Сборка хаба',
    machine: { id: workstation.id, name: 'workstation' },
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    model: 'opus-5.5',
    unread_count: 2,
    activity: {
      state: 'waiting_for_user',
      evidence: { phase: 'awaiting_permission', question: 'Bash: cargo test' },
      since: at
    } as never
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await resizeViewport(page, { width: 360, height: 740 });

  const row = page.locator(`[data-session="${id}"]`);
  await expect(row.locator('.text')).toHaveText('Ждёт разрешения · Bash · 12 мин');
  await expect(row.locator('.who')).toContainText('Opus · workstation');
  const status = await row.locator('.text').evaluate((node) => ({
    scroll: node.scrollWidth,
    client: node.clientWidth
  }));
  expect(status.scroll).toBeLessThanOrEqual(status.client + 1);
  const suffix = await row.locator('.who').evaluate((node) => ({
    scroll: node.scrollWidth,
    client: node.clientWidth
  }));
  // Хвост уступает первым: он либо обрезан, либо исчез целиком, но состояние не тронуто.
  if (suffix.client > 0) expect(suffix.scroll).toBeGreaterThan(suffix.client);
  else expect(suffix.client).toBe(0);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});
