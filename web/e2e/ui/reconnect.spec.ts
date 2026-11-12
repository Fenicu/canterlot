import type { Page, Route } from '@playwright/test';
import { test, expect } from './fixtures';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';

declare global {
  interface Window {
    reconnectSentinel?: number;
  }
}

const expired = async (route: Route) => {
  await route.fulfill({
    status: 401,
    contentType: 'application/json',
    json: {
      error: {
        code: 'unauthenticated',
        message: 'Нужен вход',
        scope: 'auth',
        retry: 'never',
        correlation_id: '00000000-0000-4000-8000-0000000000e9'
      }
    }
  });
};

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

test('a_silently_dropped_socket_is_detected_by_the_heartbeat_and_recovers_without_a_reload', async ({ page }) => {
  test.setTimeout(90_000);
  const { hub, api, ids } = await boot(page);
  await page.evaluate(() => { window.reconnectSentinel = 1; });
  // Сокет перестаёт отвечать, но close не приходит; новые попытки тоже отклоняются.
  hub.chaos.blackhole(true);
  hub.acceptConnections = false;
  await expect(page.locator('[data-banner]')).toContainText('Нет связи. Переподключаюсь…', { timeout: 45_000 });
  expect(await page.evaluate(() => window.reconnectSentinel)).toBe(1);
  hub.chaos.blackhole(false);
  hub.acceptConnections = true;
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect(page.locator('[data-banner]')).toContainText('Связь восстановлена', { timeout: 15_000 });
  expect(await page.evaluate(() => window.reconnectSentinel)).toBe(1);
  // Подписка восстановлена с прежнего курсора: событие после разрыва доходит без перезагрузки.
  const row = api.list.get(ids.pinned);
  hub.append('conversations', {
    kind: 'conversation_updated',
    conversation_id: ids.pinned,
    payload: {
      id: ids.pinned,
      title: row?.title,
      title_source: 'user',
      color: row?.color ?? null,
      pinned: false,
      archived: true,
      groups: row?.groups ?? [],
      machine: row?.machine,
      harness: row?.harness,
      workdir: row?.workdir,
      created_at: row?.created_at,
      counts: api.counts
    }
  });
  hub.flush();
  await expect(page.locator(`[data-session="${ids.pinned}"]`)).toHaveCount(0);
});

test('an_expired_session_shows_sign_in_and_recovers_after_auth_without_a_reload', async ({ page }) => {
  test.setTimeout(45_000);
  const { hub } = await boot(page);
  await page.route('**/api/v1/me', expired);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(page.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 20_000 });
  await page.evaluate(() => { window.reconnectSentinel = 2; });
  // Вошли в другой вкладке: проверка входа при возврате снимает страницу входа без перезагрузки.
  await page.unroute('**/api/v1/me', expired);
  hub.acceptConnections = true;
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect(page.getByRole('heading', { name: 'Вход в canterlot' })).toBeHidden({ timeout: 10_000 });
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  expect(await page.evaluate(() => window.reconnectSentinel)).toBe(2);
});
