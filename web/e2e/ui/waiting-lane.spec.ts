import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedWaitingSessions, type WaitingSessions } from '../../testing/fixtures/waiting-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

interface Seeded extends WaitingSessions {
  hub: FakeHub;
  api: FakeHubApi;
}

async function boot(page: Page): Promise<Seeded> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seeded = seedWaitingSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { ...seeded, hub, api };
}

/** Событие списка: сессия перестала ждать владельца и живёт в простое. */
function settle(seeded: Seeded, id: string): void {
  const row = seeded.api.list.get(id);
  seeded.hub.append('conversations', {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'idle', evidence: { last_turn_at: ago(1) }, since: ago(1) },
      unread_count: 0,
      queue: row?.queue ?? { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: ago(1),
      settings: { model: row?.model ?? 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  seeded.hub.flush();
}

test('waiting_lane_sits_on_top_oldest_first_and_keeps_both_rows_in_their_groups', async ({ page }) => {
  const { ids } = await boot(page);
  const head = page.getByRole('button', { name: 'Ждут вас · 2' });
  await expect(head).toBeVisible();
  await expect(head).toHaveAttribute('aria-expanded', 'true');

  const lane = page.locator('[data-section-slot="waiting"]');
  await expect(lane).toBeVisible();
  await expect(lane.locator('article .title')).toHaveText(['Долгий вопрос', 'Свежее разрешение']);
  // Счёт раздела — отдельное число, подпись читается вместе с ним.
  await expect(head.locator('.count')).toHaveText('2');
  // Копии: строка стоит и в разделе, и в своей группе.
  await expect(page.locator(`[data-waiting="${ids.older}"]`)).toBeVisible();
  await expect(page.locator(`[data-session="${ids.older}"]`)).toBeVisible();
  await expect(page.locator(`[data-waiting="${ids.newer}"]`)).toBeVisible();
  await expect(page.locator(`[data-session="${ids.newer}"]`)).toBeVisible();
  // Работающая сессия раздела не занимает.
  await expect(lane).not.toContainText('Сборка идёт');

  const laneTop = (await lane.boundingBox())!;
  const groupTop = (await page.getByRole('button', { name: /^Дом/ }).boundingBox())!;
  expect(laneTop.y).toBeLessThan(groupTop.y);
});

test('waiting_lane_disappears_when_nobody_waits_and_the_rows_stay', async ({ page }) => {
  const seeded = await boot(page);
  const { ids } = seeded;
  settle(seeded, ids.older);
  settle(seeded, ids.newer);
  await expect(page.locator('[data-section-slot="waiting"]')).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^Ждут вас/ })).toHaveCount(0);
  // Строки никуда не ушли: они по-прежнему в своих группах.
  await expect(page.locator(`[data-session="${ids.older}"]`)).toBeVisible();
  await expect(page.locator(`[data-session="${ids.newer}"]`)).toBeVisible();
});

test('chat_back_counts_the_other_waiters_and_opens_the_lane', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'на столе список всегда рядом: счёт ведёт полоса групп');
  const { ids } = await boot(page);
  await page.goto(`/c/${ids.current}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  const back = page.getByRole('link', { name: 'Назад, ждут ответа: 2' });
  await expect(back).toBeVisible();
  await expect(back).toContainText('2');
  // Своя сессия в счёт не идёт, а чужое ожидание снимается событием, без перезагрузки.
  await expect(page.getByRole('link', { name: 'Назад, ждут ответа: 3' })).toHaveCount(0);

  await back.click();
  await expect(page).toHaveURL(/\/sessions$/);
  const head = page.getByRole('button', { name: 'Ждут вас · 2' });
  await expect(head).toBeVisible();
  const box = (await page.locator('[data-section-slot="waiting"]').boundingBox())!;
  const height = page.viewportSize()!.height;
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeLessThan(height / 2);
});

test('chat_back_count_follows_a_session_that_stops_waiting', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'счёт на стрелке есть только на телефоне');
  const seeded = await boot(page);
  const { ids } = seeded;
  await page.goto(`/c/${ids.current}`);
  await expect(page.getByRole('link', { name: 'Назад, ждут ответа: 2' })).toBeVisible();
  settle(seeded, ids.newer);
  await expect(page.getByRole('link', { name: 'Назад, ждут ответа: 1' })).toBeVisible();
  settle(seeded, ids.older);
  await expect(page.getByRole('link', { name: 'Все сессии' })).toBeVisible();
});

test('desktop_sidebar_shows_the_lane_and_an_amber_all_badge', async ({ page, isMobile }) => {
  test.skip(isMobile, 'полоса групп есть только на столе');
  await page.setViewportSize({ width: 1440, height: 900 });
  await boot(page);
  const rail = page.locator('[data-group-rail]');
  await expect(rail.getByRole('tab', { name: 'Все 4, ждут вас: 2' })).toBeVisible();
  const badge = rail.getByRole('tab', { name: /^Все/ }).locator('.count');
  await expect(badge).toHaveAttribute('data-tone', 'warning');
  // Янтарь — фон счётчика, а смысл несут слова раздела и его счёт.
  const amber = await page.evaluate(() => {
    const probe = document.createElement('span');
    probe.style.background = 'var(--warning)';
    document.body.append(probe);
    const colour = getComputedStyle(probe).backgroundColor;
    probe.remove();
    return colour;
  });
  expect(await badge.evaluate((node) => getComputedStyle(node).backgroundColor)).toBe(amber);
  await expect(page.getByRole('button', { name: 'Ждут вас · 2' })).toBeVisible();
  await expect(page.locator('[data-pane="list"] [data-section-slot="waiting"]')).toBeVisible();
});
