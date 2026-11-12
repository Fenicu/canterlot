import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, resizeViewport, test } from './fixtures';

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

test('desktop_three_pane_layout_at_1440_and_two_pane_at_1024', async ({ page }) => {
  const { api, ids } = await boot(page);
  const block = api.id();
  api.blocks.set(block, { text: 'поручение', rev: 1, state: 'final' });
  api.histories.set(ids.pinned, [{ kind: 'block', id: block, block_kind: 'tool_call', content: 'поручение', content_bytes: 20, content_truncated: false, created_seq: '1', created_at: new Date().toISOString(), rev: 1, state: 'final', agent: 'claude', model: 'opus-5.5', tool_name: 'Agent', summary: 'поручение', parent_block_id: null, input_command_id: null, line_count: 1, started_at: null, finished_at: null }] as never);
  api.bindConversation(ids.pinned);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`/c/${ids.pinned}`);
  await expect(page.locator('[data-layout="desktop"]')).toBeVisible();
  await expect(page.locator('[data-pane="list"]')).toBeVisible();
  await expect(page.locator('[data-pane="chat"]')).toBeVisible();
  await page.getByRole('button', { name: 'Делегирование' }).click();
  await expect(page.locator('[data-pane="lane"]')).toBeVisible();
  await page.setViewportSize({ width: 1024, height: 800 });
  await expect(page.locator('[data-layout="tablet"]')).toBeVisible();
  await expect(page.locator('[data-pane="lane"]')).toHaveCount(0);
  await expect(page.locator('[data-pane="list"]')).toBeVisible();
  await expect(page.locator('[data-pane="chat"]')).toBeVisible();
});

test('phone_document_does_not_scroll_and_dock_stays_in_viewport_after_reload', async ({ page }) => {
  test.skip(test.info().project.name !== 'phone', 'проверка телефонной раскладки');
  await boot(page);
  const probe = () => page.evaluate(() => {
    const root = document.scrollingElement as HTMLElement;
    const dock = document.querySelector('.dock')!.getBoundingClientRect();
    return {
      scrollTop: root.scrollTop,
      scrollHeight: root.scrollHeight,
      innerHeight: window.innerHeight,
      dockBottom: dock.bottom,
      shellPosition: getComputedStyle(document.querySelector('.shell')!).position,
      overflow: getComputedStyle(document.documentElement).overflowY,
      overscroll: getComputedStyle(document.documentElement).overscrollBehaviorY
    };
  });
  const expectPinned = async () => {
    const state = await probe();
    expect(state.scrollTop).toBe(0);
    expect(state.scrollHeight).toBeLessThanOrEqual(state.innerHeight);
    expect(state.dockBottom).toBeLessThanOrEqual(state.innerHeight);
    expect(state.shellPosition).toBe('fixed');
    expect(state.overflow).toBe('hidden');
    expect(state.overscroll).toBe('none');
  };
  await expectPinned();
  await page.evaluate(() => window.scrollTo(0, 500));
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.dock')).toBeVisible();
  await expectPinned();
  await resizeViewport(page, { width: 384, height: 832 });
  // Высота оболочки (--app-height) догоняет новое окно кадром позже resize.
  await expect(expectPinned).toPass();
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expectPinned();
});

test('connection_banner_follows_transport_state_and_announces_politely', async ({ page }) => {
  const { hub } = await boot(page);
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(page.locator('[data-banner]')).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  await expect(page.locator('[aria-live="polite"]')).toContainText('Нет связи. Переподключаюсь…');
  hub.acceptConnections = true;
  await expect(page.locator('[data-banner]')).toContainText('Связь восстановлена', { timeout: 20_000 });
  for (let attempt = 0; attempt < 3; attempt += 1) {
    hub.chaos.forceResync('cursor_ahead');
    await page.waitForTimeout(1000);
  }
  await expect(page.locator('[data-banner]')).toContainText('Не удаётся синхронизироваться. Повторяю попытки', { timeout: 8_000 });
  await expect(page.getByRole('button', { name: 'Обновить' })).toBeVisible();
});

test('an_unauthenticated_answer_replaces_the_app_with_the_sign_in_page', async ({ page }) => {
  await boot(page);
  await page.route('**/api/v1/conversations?*', async (route) => {
    await route.fulfill({
      status: 401,
      contentType: 'application/json',
      json: { error: { code: 'unauthenticated', message: 'Нужен вход', scope: 'auth', retry: 'never', correlation_id: '00000000-0000-4000-8000-0000000000e4' } }
    });
  });
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('поток');
  await expect(page.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 5_000 });
  await expect(page.getByRole('textbox', { name: 'Логин' })).toHaveAttribute('autocomplete', 'username');
});
