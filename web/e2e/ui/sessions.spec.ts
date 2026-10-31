import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions, type MockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, empty = false) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = empty ? null : seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids: ids as MockupSessions };
}

test('list_matches_the_mockup_fixture_structure', async ({ page }) => {
  await boot(page);
  await expect(page.getByRole('link', { name: /Восстановление потока/ })).toBeVisible();
  await expect(page.getByRole('link', { name: /Очередь сообщений/ })).toBeVisible();
  await expect(page.getByText('Есть непрочитанное').first()).toBeVisible();
  await expect(page.getByText('Claude Code · opus-5.5 · workstation').first()).toBeVisible();
  await expect(page.getByText('Генерирует').first()).toBeVisible();
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.getByRole('link', { name: /Восстановление потока/ })).toBeVisible();
});

test('group_tabs_are_a_tablist_with_arrow_navigation_and_stay_inside_the_pill', async ({ page, isMobile }) => {
  await boot(page);
  const tablist = page.getByRole('tablist', { name: 'Группы сессий' });
  const all = tablist.getByRole('tab', { name: /Все/ });
  await all.focus();
  await all.press('ArrowRight');
  await expect(tablist.getByRole('tab', { name: /Закреплённые/ })).toHaveAttribute('aria-selected', 'true');
  const metrics = await tablist.evaluate((node) => ({
    scroll: node.scrollWidth,
    client: node.clientWidth,
    page: document.documentElement.scrollWidth
  }));
  // Телефон листает вкладки пальцем внутри капсулы, боковая панель переносит их на новую строку.
  if (isMobile) expect(metrics.scroll).toBeGreaterThan(metrics.client);
  else expect(metrics.scroll).toBeLessThanOrEqual(metrics.client);
  expect(metrics.page).toBeLessThanOrEqual((page.viewportSize()?.width ?? metrics.page) + 1);
});

test('sections_collapse_and_remember_state', async ({ page }) => {
  await boot(page);
  const section = page.getByRole('button', { name: /bookshelf 4/ });
  await section.click();
  await expect(section).toHaveAttribute('aria-expanded', 'false');
  await expect(page.getByRole('link', { name: /Поиск по библиотеке/ })).toHaveCount(0);
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(page.getByRole('button', { name: /bookshelf 4/ })).toHaveAttribute('aria-expanded', 'false');
});

test('search_debounces_and_queries_the_server_with_the_typed_text', async ({ page }) => {
  await boot(page);
  const seen: string[] = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (url.pathname === '/api/v1/conversations' && url.searchParams.has('q')) seen.push(url.searchParams.get('q') ?? '');
  });
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('100%_поток');
  await expect.poll(() => seen.at(-1), { timeout: 5_000 }).toBe('100%_поток');
});

test('search_without_results_offers_reset', async ({ page }) => {
  await boot(page);
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('неттакого');
  await expect(page.getByText('Ничего не найдено')).toBeVisible();
  await page.getByRole('button', { name: 'Сбросить' }).first().click();
  await expect(page.getByRole('link', { name: /Восстановление потока/ })).toBeVisible();
});

test('pin_colour_archive_roundtrip_updates_the_list_with_idempotent_calls', async ({ page }) => {
  const { ids } = await boot(page);
  const keys: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'PATCH' && request.url().includes(ids.plain)) keys.push(request.headers()['idempotency-key'] ?? '');
  });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('switch', { name: 'Закрепить' }).click();
  await expect.poll(() => keys.length).toBeGreaterThan(0);
  expect(keys[0]).not.toBe('');
  await expect(page.getByRole('button', { name: /Закреплённые 3/ })).toBeVisible();
  await page.getByRole('radio', { name: 'Синий' }).check();
  await expect(page.locator(`[data-session="${ids.plain}"] .mark`)).toHaveAttribute('data-token', '--blue');
});

test('group_manager_create_rename_reorder_delete_keeps_sessions', async ({ page }) => {
  await boot(page);
  await page.getByRole('button', { name: 'Ещё' }).click();
  await page.getByRole('menuitem', { name: 'Группы' }).click();
  await page.getByRole('textbox', { name: 'Название группы' }).fill('черновики');
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page.getByRole('tab', { name: /черновики/ })).toBeVisible();
  const renamed = page.getByRole('textbox', { name: 'Название черновики' });
  await renamed.fill('заметки');
  await renamed.press('Enter');
  await expect(page.getByRole('tab', { name: /заметки/ })).toBeVisible();
  await page.getByRole('button', { name: 'Выше' }).last().click();
  await page.getByRole('button', { name: 'Удалить' }).last().click();
  await expect(page.getByText('Сессии останутся в списке')).toBeVisible();
  await page.getByRole('button', { name: 'Подтвердить удаление' }).click();
  await expect(page.getByRole('link', { name: /Поиск по библиотеке/ })).toBeVisible();
});

test('session_sheet_edits_title_colour_groups_and_archive', async ({ page }) => {
  await boot(page);
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  const title = page.getByRole('textbox', { name: 'Название' });
  await title.fill('Доставка вручную');
  await title.press('Enter');
  await expect(page.getByRole('link', { name: /Доставка вручную/ })).toBeVisible();
  await page.getByRole('checkbox', { name: 'canterlot' }).check();
  await expect(page.getByRole('tab', { name: /canterlot 4/ })).toBeVisible();
});

test('archive_with_active_crunch_asks_for_confirmation_and_retries_with_force', async ({ page }) => {
  const { api, ids } = await boot(page);
  const row = api.list.get(ids.plain);
  if (row) row.crunch.enabled = true;
  const bodies: { key: string; force?: boolean }[] = [];
  page.on('request', (request) => {
    if (request.method() !== 'PATCH' || !request.url().includes(ids.plain)) return;
    const body = request.postDataJSON() as { force?: boolean; archived?: boolean };
    if (body.archived) bodies.push({ key: request.headers()['idempotency-key'] ?? '', force: body.force });
  });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /В архив/ }).click();
  await expect(page.getByText('кранч')).toBeVisible();
  await page.getByRole('button', { name: 'Всё равно архивировать' }).click();
  await expect.poll(() => bodies.length).toBe(2);
  expect(bodies[1]?.force).toBe(true);
  expect(bodies[0]?.key).not.toBe(bodies[1]?.key);
});

test('session_menu_closes_on_tab_change', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально: вкладки под ним недоступны');
  await boot(page);
  const tablist = page.getByRole('tablist', { name: 'Группы сессий' });
  const menu = page.getByRole('switch', { name: 'Закрепить' });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await expect(menu).toBeVisible();
  await tablist.getByRole('tab', { name: /Закреплённые/ }).click();
  await expect(menu).toHaveCount(0);
  await tablist.getByRole('tab', { name: /Все/ }).click();
  await expect(menu).toHaveCount(0);
});

test('session_menu_closes_when_the_search_changes_or_the_route_changes', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально: поиск и переход под ним недоступны');
  await boot(page);
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  const menu = page.getByRole('switch', { name: 'Закрепить' });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await expect(menu).toBeVisible();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('очередь');
  await expect(menu).toHaveCount(0);
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('');
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await expect(menu).toBeVisible();
  await page.getByRole('link', { name: 'Новая сессия' }).click();
  await expect(page).toHaveURL(/\/new$/);
  await expect(menu).toHaveCount(0);
});

test('session_menu_closes_when_the_session_leaves_the_list_from_another_device', async ({ page }) => {
  const { hub, api, ids } = await boot(page);
  const menu = page.getByRole('switch', { name: 'Закрепить' });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await expect(menu).toBeVisible();
  const row = api.list.get(ids.plain);
  hub.append('conversations', {
    kind: 'conversation_updated',
    conversation_id: ids.plain,
    payload: {
      id: ids.plain,
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
  await expect(menu).toHaveCount(0);
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toHaveCount(0);
});

test('delete_asks_for_confirmation_naming_the_session_and_removes_it_only_after_the_hub_confirms', async ({ page }) => {
  const { ids } = await boot(page);
  const row = page.locator(`[data-session="${ids.plain}"]`);
  const keys: string[] = [];
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route(`**/api/v1/conversations/${ids.plain}`, async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback();
    keys.push(route.request().headers()['idempotency-key'] ?? '');
    await held;
    await route.fallback();
  });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /^Удалить/ }).click();
  const dialog = page.getByRole('dialog', { name: 'Удалить сессию?' });
  await expect(dialog).toContainText('Повторная доставка ввода');
  await expect(dialog).toContainText('Это нельзя отменить');
  await expect(dialog).toContainText('История CLI');
  await dialog.getByRole('button', { name: 'Отмена' }).click();
  await expect(row).toBeVisible();
  expect(keys).toEqual([]);

  await page.getByRole('button', { name: /^Удалить/ }).click();
  await page.getByRole('dialog', { name: 'Удалить сессию?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect.poll(() => keys.length).toBe(1);
  await expect(row).toBeVisible();
  release();
  await expect(row).toHaveCount(0);
  await expect(page.getByRole('switch', { name: 'Закрепить' })).toHaveCount(0);
  expect(keys[0]).not.toBe('');
});

test('delete_while_live_shows_the_refusal_and_keeps_the_session', async ({ page }) => {
  const { api, ids } = await boot(page);
  const detail = api.details.get(ids.plain);
  if (detail) detail.execution = { state: 'running', generation: 1 };
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /^Удалить/ }).click();
  await page.getByRole('dialog', { name: 'Удалить сессию?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'Сначала остановите сессию' })).toBeVisible();
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toBeVisible();
});

test('a_session_deleted_on_another_device_disappears_from_the_list_and_the_counts', async ({ page }) => {
  const { api, ids } = await boot(page);
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await expect(page.getByRole('switch', { name: 'Закрепить' })).toBeVisible();
  const before = api.counts.all;
  api.deleteConversation(ids.plain);
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toHaveCount(0);
  await expect(page.getByRole('switch', { name: 'Закрепить' })).toHaveCount(0);
  await expect(page.getByRole('tab', { name: new RegExp(`Все ${before - 1}`) })).toBeVisible();
});

test('an_open_chat_of_a_deleted_session_ends_on_a_sensible_screen', async ({ page }) => {
  const { api, ids } = await boot(page);
  await page.goto(`/c/${ids.plain}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  api.deleteConversation(ids.plain);
  await expect(page.getByRole('heading', { name: 'Сессия удалена' })).toBeVisible();
  await page.getByRole('link', { name: 'К списку сессий' }).click();
  await expect(page).toHaveURL(/\/sessions$/);
});

test('opening_a_deleted_session_does_not_retry_forever', async ({ page }) => {
  const { api, ids } = await boot(page);
  api.deleteConversation(ids.plain);
  let snapshots = 0;
  page.on('request', (request) => {
    if (request.url().endsWith(`/conversations/${ids.plain}/snapshot`)) snapshots += 1;
  });
  await page.goto(`/c/${ids.plain}`);
  await expect(page.getByRole('heading', { name: 'Сессия удалена' })).toBeVisible({ timeout: 15_000 });
  await page.waitForTimeout(1500);
  expect(snapshots).toBe(1);
});

test('delete_from_the_open_chat_returns_to_the_list', async ({ page }) => {
  const { ids } = await boot(page);
  await page.goto(`/c/${ids.plain}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Название, цвет и группы' }).click();
  await page.getByRole('button', { name: /^Удалить/ }).click();
  await page.getByRole('dialog', { name: 'Удалить сессию?' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect(page).toHaveURL(/\/sessions$/);
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toHaveCount(0);
});

test('unread_dot_and_counts_update_from_events', async ({ page }) => {
  const { hub, api, ids } = await boot(page);
  const row = api.list.get(ids.plain);
  hub.append('conversations', {
    kind: 'derived_changed',
    conversation_id: ids.plain,
    payload: {
      conversation_id: ids.plain,
      activity: { state: 'idle', evidence: {}, since: '2026-10-28T12:00:00.000Z' },
      unread_count: 3,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: '2026-10-28T12:00:00.000Z',
      settings: { model: row?.model ?? 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.flush();
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toContainText('Непрочитанных: 3');
  api.counts = { ...api.counts, all: 11 };
  hub.append('conversations', {
    kind: 'conversation_updated',
    conversation_id: ids.plain,
    payload: {
      id: ids.plain,
      title: row?.title,
      title_source: 'user',
      color: row?.color ?? null,
      pinned: false,
      archived: false,
      groups: row?.groups ?? [],
      machine: row?.machine,
      harness: row?.harness,
      workdir: row?.workdir,
      created_at: row?.created_at,
      counts: api.counts
    }
  });
  hub.flush();
  await expect(page.getByRole('tab', { name: /Все 11/ })).toBeVisible();
});

test('phone_navigation_list_chat_back_preserves_list_scroll', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 500 });
  await boot(page);
  const scroller = page.locator('[data-list-scroll]');
  await scroller.evaluate((node) => {
    node.scrollTop = 180;
  });
  await expect.poll(() => scroller.evaluate((node) => node.scrollTop)).toBeGreaterThan(100);
  await page.getByRole('link', { name: /Глава/ }).click();
  await page.getByRole('link', { name: 'Все сессии' }).click();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect.poll(() => scroller.evaluate((node) => node.scrollTop)).toBeGreaterThan(100);
});

test('limits_strip_warns_at_80_and_shows_the_reserve_variant', async ({ page }) => {
  const { hub, api, ids } = await boot(page);
  await expect(page.getByText(/96%/)).toBeVisible();
  await expect(page.locator('.limits[data-tone="warning"]')).toBeVisible();
  const claude = api.limits.find((item) => item.provider === 'claude');
  if (claude) claude.weekly_reserve_active = true;
  const row = api.list.get(ids.pinned);
  if (row) row.crunch.enabled = true;
  hub.append('conversations', {
    kind: 'derived_changed',
    conversation_id: ids.pinned,
    payload: {
      conversation_id: ids.pinned,
      activity: row?.activity ?? { state: 'idle', evidence: {}, since: '2026-10-28T12:00:00.000Z' },
      unread_count: row?.unread_count ?? 0,
      queue: row?.queue ?? { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: row?.last_activity_at ?? null,
      settings: { model: row?.model ?? 'opus', effort: row?.effort ?? 'medium', permission_mode: row?.permission_mode ?? 'owner' },
      crunch: { enabled: true }
    }
  });
  hub.append('conversations', {
    kind: 'limits_changed',
    payload: {
      provider: 'claude',
      window: 'weekly',
      used_permille: 960,
      resets_at: claude?.weekly?.resets_at ?? null,
      observed_at: claude?.weekly?.observed_at ?? new Date().toISOString(),
      weekly_reserve_active: true,
      weekly_reserve_resets_at: null
    }
  });
  hub.flush();
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`/c/${ids.pinned}`);
  const strip = page.locator('[data-pane="list"] .limits summary');
  await expect(strip).toContainText('Claude 5ч');
  await expect(strip).toContainText('неделя 96%');
  await expect(strip).toContainText('Codex 5ч');
  await expect(strip.locator('.reserve')).toHaveText(['резерв']);
});

test('empty_states_offer_actions', async ({ page }) => {
  await boot(page, true);
  await expect(page.getByText(/machine add/)).toBeVisible();
  await expect(page.getByRole('link', { name: 'Создать первую сессию' })).toBeVisible();
});

test('keyboard_reaches_every_list_action', async ({ page }) => {
  await boot(page);
  await page.getByRole('button', { name: 'Поиск', exact: true }).focus();
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', { name: 'Сменить тему' })).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', { name: 'Ещё' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('menuitem', { name: 'Группы' })).toBeVisible();
  await page.getByRole('link', { name: /Восстановление потока/ }).focus();
  await expect(page.getByRole('link', { name: /Восстановление потока/ })).toBeFocused();
});

test('reconnect_during_list_resync_keeps_rows_stable', async ({ page }) => {
  const { hub } = await boot(page);
  const before = await page.getByRole('link', { name: /Восстановление потока/ }).textContent();
  hub.chaos.restart();
  hub.chaos.forceResync('cursor_ahead');
  await expect(page.getByRole('link', { name: /Восстановление потока/ })).toHaveText(before ?? '');
});
