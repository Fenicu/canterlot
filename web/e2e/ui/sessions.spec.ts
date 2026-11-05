import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions, type MockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, fillContrast, test } from './fixtures';

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

test('selected_group_tab_stands_out_in_both_themes', async ({ page }) => {
  await boot(page);
  const selected = page.getByRole('tablist', { name: 'Группы сессий' }).getByRole('tab', { selected: true });
  // Признак выбора — не текст, поэтому ему нужно 3:1 с фоном под ним (WCAG 1.4.11).
  expect(await fillContrast(selected)).toBeGreaterThanOrEqual(3);
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  expect(await fillContrast(selected)).toBeGreaterThanOrEqual(3);
});

test('phone_tabs_fade_only_the_edge_with_more_tabs_and_keep_the_selected_tab_clear', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'на рабочем столе вкладки переносятся, а не листаются');
  await boot(page);
  const tablist = page.getByRole('tablist', { name: 'Группы сессий' });
  await expect(tablist).toHaveAttribute('data-fade-end', '');
  await expect(tablist).not.toHaveAttribute('data-fade-start', '');
  await tablist.getByRole('tab', { name: /Архив/ }).click();
  await expect(tablist).toHaveAttribute('data-fade-start', '');
  await expect(tablist).not.toHaveAttribute('data-fade-end', '');
  // Выбранная вкладка целиком вне гаснущей полосы.
  const strip = (await tablist.boundingBox())!;
  const tab = (await tablist.getByRole('tab', { selected: true }).boundingBox())!;
  expect(tab.x + tab.width).toBeLessThanOrEqual(strip.x + strip.width + 1);
  expect(tab.x).toBeGreaterThanOrEqual(strip.x + 18);
});

test('tabs_of_the_desktop_list_stay_compact_and_left_aligned', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефон листает вкладки');
  await page.setViewportSize({ width: 1707, height: 960 });
  await boot(page);
  const boxes = await page.getByRole('tab').evaluateAll((tabs) => tabs.map((tab) => tab.getBoundingClientRect()).map((box) => ({ width: box.width, top: box.top })));
  expect(Math.max(...boxes.map((box) => box.width))).toBeLessThan(220);
  const list = await page.locator('[data-pane="list"]').boundingBox();
  expect(boxes[0]!.width).toBeLessThan((list?.width ?? 0) / 2);
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

test('the_last_selected_tab_survives_a_reload_and_a_deleted_group_falls_back_to_all', async ({ page }) => {
  const { api, ids } = await boot(page);
  const tablist = page.getByRole('tablist', { name: 'Группы сессий' });
  await tablist.getByRole('tab', { name: /bookshelf/ }).click();
  await expect(tablist.getByRole('tab', { name: /bookshelf/ })).toHaveAttribute('aria-selected', 'true');
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(tablist.getByRole('tab', { name: /bookshelf/ })).toHaveAttribute('aria-selected', 'true');

  api.groups = api.groups.filter((group) => group.id !== ids.books);
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(tablist.getByRole('tab', { name: /bookshelf/ })).toHaveCount(0);
  await expect(tablist.getByRole('tab', { name: /Все/ })).toHaveAttribute('aria-selected', 'true');
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
  // Сброс один: в строке поиска, без повтора под «Ничего не найдено».
  await expect(page.getByRole('button', { name: 'Сбросить' })).toHaveCount(1);
  await page.getByRole('button', { name: 'Сбросить' }).click();
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
  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  const dialog = page.getByRole('dialog', { name: 'Переместить сессию в корзину?' });
  await expect(dialog).toContainText('Повторная доставка ввода');
  await expect(dialog).toContainText('можно вернуть');
  await expect(dialog).toContainText('История CLI');
  await dialog.getByRole('button', { name: 'Отмена' }).click();
  await expect(row).toBeVisible();
  expect(keys).toEqual([]);

  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  await page.getByRole('dialog', { name: 'Переместить сессию в корзину?' }).getByRole('button', { name: 'В корзину', exact: true }).click();
  await expect.poll(() => keys.length).toBe(1);
  await expect(row).toBeVisible();
  release();
  await expect(row).toHaveCount(0);
  await expect(page.getByRole('switch', { name: 'Закрепить' })).toHaveCount(0);
  expect(keys[0]).not.toBe('');
});

function idleProcess(api: FakeHubApi, id: string): void {
  const row = api.list.get(id);
  if (row) row.activity = { state: 'idle', evidence: { last_turn_at: '2026-10-28T19:00:00.000Z' }, since: '2026-10-28T19:00:00.000Z' };
  const detail = api.details.get(id);
  if (detail) {
    detail.activity = row?.activity;
    detail.execution = { state: 'running', generation: 1 };
  }
}

function turnRunning(api: FakeHubApi, id: string): void {
  idleProcess(api, id);
  const activity = { state: 'working', evidence: { phase: 'running_tool', summary: 'cargo test', phase_since: '2026-10-28T19:00:00.000Z' }, since: '2026-10-28T19:00:00.000Z' };
  const row = api.list.get(id);
  if (row) row.activity = activity;
  const detail = api.details.get(id);
  if (detail) detail.activity = activity;
}

test('delete_of_a_session_refused_by_the_hub_shows_the_refusal_and_keeps_it', async ({ page }) => {
  const { api, ids } = await boot(page);
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  const dialog = page.getByRole('dialog', { name: 'Переместить сессию в корзину?' });
  await expect(dialog).toBeVisible();
  const detail = api.details.get(ids.plain);
  if (detail) detail.execution = { state: 'running', generation: 1 };
  await dialog.getByRole('button', { name: 'В корзину', exact: true }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'Сначала остановите сессию' })).toBeVisible();
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toBeVisible();
});

test('delete_of_an_idle_session_stops_its_process_first_and_says_so', async ({ page }) => {
  const { api, ids } = await boot(page);
  idleProcess(api, ids.plain);
  const calls: string[] = [];
  page.on('request', (request) => {
    if (!request.url().includes(ids.plain)) return;
    if (request.url().endsWith('/stop')) calls.push(`stop:${request.headers()['idempotency-key'] ?? ''}`);
    if (request.method() === 'DELETE') calls.push('delete');
  });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  const dialog = page.getByRole('dialog', { name: 'Остановить и убрать в корзину?' });
  await expect(dialog).toContainText('Процесс агента на машине workstation будет остановлен');
  await expect(dialog).toContainText('можно вернуть');
  await dialog.getByRole('button', { name: 'Остановить и убрать' }).click();
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toHaveCount(0);
  expect(calls).toHaveLength(2);
  expect(calls[0]).toMatch(/^stop:.+/);
  expect(calls[1]).toBe('delete');
});

test('delete_during_a_turn_is_blocked_and_names_what_is_running', async ({ page }) => {
  const { api, ids } = await boot(page);
  turnRunning(api, ids.plain);
  const calls: string[] = [];
  page.on('request', (request) => {
    if (request.url().includes(ids.plain) && (request.url().endsWith('/stop') || request.method() === 'DELETE')) calls.push(request.url());
  });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'Сначала остановите работу: выполняет cargo test' })).toBeVisible();
  await expect(page.getByRole('dialog', { name: /сессию\?/ })).toHaveCount(0);
  expect(calls).toEqual([]);
});

test('idle_session_with_a_live_process_archives_without_a_warning', async ({ page }) => {
  const { api, ids } = await boot(page);
  idleProcess(api, ids.plain);
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /В архив/ }).click();
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Всё равно архивировать' })).toHaveCount(0);
});

test('archive_warning_says_what_is_running', async ({ page }) => {
  const { api, ids } = await boot(page);
  turnRunning(api, ids.plain);
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /В архив/ }).click();
  await expect(page.getByText('Сначала подтвердите: выполняет cargo test')).toBeVisible();
  await expect(page.getByText('идёт исполнение')).toHaveCount(0);
});

test('session_menu_opens_whole_and_above_the_row_at_any_list_position', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально и закрывает весь экран');
  await page.setViewportSize({ width: 1707, height: 960 });
  await boot(page);
  const actions = page.getByRole('button', { name: /^Действия:/ });
  const last = (await actions.count()) - 1;
  for (const [width, height] of [[1707, 960], [388, 760], [388, 587]] as const) {
    await page.setViewportSize({ width, height });
    for (const index of [0, last]) {
      await actions.nth(index).scrollIntoViewIfNeeded();
      await actions.nth(index).click();
      const report = await page.locator('.popover').evaluate(async (panel) => {
        await new Promise((done) => setTimeout(done, 250));
        const box = panel.getBoundingClientRect();
        const list = document.querySelector('.list')!.getBoundingClientRect();
        const field = panel.querySelector('input')!.getBoundingClientRect();
        const covered = document.elementFromPoint(field.left + field.width / 2, field.top + field.height / 2);
        return { top: box.top - list.top, bottom: list.bottom - box.bottom, scrolled: panel.scrollTop, fieldOnTop: covered === panel.querySelector('input'), overflow: panel.scrollHeight - panel.clientHeight, room: list.height };
      });
      expect(report.top).toBeGreaterThanOrEqual(0);
      expect(report.bottom).toBeGreaterThanOrEqual(-1);
      expect(report.scrolled).toBe(0);
      expect(report.fieldOnTop).toBe(true);
      // Внутренняя прокрутка остаётся только там, где меню выше самого списка.
      if (report.room > 560) expect(report.overflow).toBeLessThanOrEqual(1);
      await page.getByRole('button', { name: 'Закрыть' }).click();
    }
  }
});

test('session_menu_of_the_last_row_goes_up_and_stays_whole_with_the_archive_warning', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально и закрывает весь экран');
  await page.setViewportSize({ width: 1707, height: 960 });
  const { api, ids } = await boot(page);
  turnRunning(api, ids.plain);
  const row = page.locator(`[data-session="${ids.plain}"]`);
  await row.scrollIntoViewIfNeeded();
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await page.getByRole('button', { name: /В архив/ }).click();
  await expect(page.getByRole('button', { name: 'Всё равно архивировать' })).toBeVisible();
  const report = await page.locator('.popover').evaluate(async (panel) => {
    await new Promise((done) => setTimeout(done, 200));
    const box = panel.getBoundingClientRect();
    const list = document.querySelector('.list')!.getBoundingClientRect();
    return { below: list.bottom - box.bottom, overflow: panel.scrollHeight - panel.clientHeight };
  });
  expect(report.below).toBeGreaterThanOrEqual(-1);
  expect(report.overflow).toBeLessThanOrEqual(1);
});

test('session_menu_closes_on_an_outside_click_however_it_was_opened', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально: его проверяет отдельный тест');
  await page.setViewportSize({ width: 1707, height: 1400 });
  const { ids } = await boot(page);
  const row = page.locator(`[data-session="${ids.plain}"]`);
  const menu = page.getByRole('dialog', { name: 'Сессия' });
  await row.getByRole('link').click({ button: 'right' });
  await expect(menu).toBeVisible();
  await page.locator('.brand').click();
  await expect(menu).toHaveCount(0);
  await row.getByRole('button').click();
  await expect(menu).toBeVisible();
  await page.locator('.brand').click({ button: 'right' });
  await expect(menu).toHaveCount(0);
  await row.getByRole('button').click();
  await expect(menu).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
  await row.getByRole('button').click();
  await expect(menu).toBeVisible();
  await row.getByRole('button').click();
  await expect(menu).toHaveCount(0);
});

test('clicks_inside_the_session_menu_and_its_confirmation_keep_it_open', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально: его проверяет отдельный тест');
  const { ids } = await boot(page);
  await page.locator(`[data-session="${ids.plain}"]`).getByRole('button').click();
  const menu = page.getByRole('dialog', { name: 'Сессия' });
  await menu.getByRole('textbox', { name: 'Название' }).click();
  await menu.getByRole('switch', { name: 'Закрепить' }).click();
  await menu.getByText('Настройки сессии').click();
  await expect(menu).toBeVisible();
  await menu.getByRole('button', { name: /^Переместить в корзину/ }).click();
  const confirm = page.getByRole('dialog', { name: 'Переместить сессию в корзину?' });
  await confirm.locator('p').first().click();
  await page.keyboard.press('Escape');
  await expect(confirm).toHaveCount(0);
  await expect(menu).toBeVisible();
  await menu.getByRole('button', { name: /^Переместить в корзину/ }).click();
  await page.mouse.click(4, 4);
  await expect(confirm).toHaveCount(0);
  await expect(menu).toBeVisible();
});

test('another_row_takes_over_from_the_open_session_menu', async ({ page, isMobile }) => {
  test.skip(isMobile, 'телефонное меню модально: строки под ним недоступны');
  await page.setViewportSize({ width: 1707, height: 1400 });
  await boot(page);
  const rows = page.locator('[data-session]');
  const first = rows.first();
  const last = rows.last();
  await first.getByRole('button').click();
  const menu = page.getByRole('dialog', { name: 'Сессия' });
  const name = menu.getByRole('textbox', { name: 'Название' });
  await expect(name).toHaveValue((await first.locator('.title').textContent()) ?? '');
  await last.getByRole('link').click({ button: 'right' });
  await expect(name).toHaveValue((await last.locator('.title').textContent()) ?? '');
  await first.getByRole('link').click({ button: 'right' });
  await expect(name).toHaveValue((await first.locator('.title').textContent()) ?? '');
  const id = await last.getAttribute('data-session');
  await last.getByRole('link').click();
  await expect(menu).toHaveCount(0);
  await expect(page).toHaveURL(new RegExp(`/c/${id}`));
});

for (const way of ['outside', 'escape'] as const) {
  test(`title_typed_in_the_menu_is_saved_when_it_closes_by_${way}`, async ({ page, isMobile }) => {
    const { ids } = await boot(page);
    const titles: string[] = [];
    page.on('request', (request) => {
      if (request.method() !== 'PATCH' || !request.url().includes(ids.plain)) return;
      const body = request.postDataJSON() as { title?: string };
      if (body.title) titles.push(body.title);
    });
    await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
    const field = page.getByRole('textbox', { name: 'Название' });
    await field.fill('Новое имя');
    if (way === 'escape') await field.press('Escape');
    else if (isMobile) await page.touchscreen.tap(195, 12);
    else await page.locator('.brand').click();
    await expect(field).toHaveCount(0);
    await expect(page.getByRole('link', { name: /Новое имя/ })).toBeVisible();
    expect(titles).toEqual(['Новое имя']);
  });
}

test('sheet_closes_on_a_tap_over_its_backdrop_and_not_inside', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'на рабочем столе меню сессии не лист');
  await boot(page);
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  const sheet = page.getByRole('dialog', { name: 'Меню сессии' });
  await sheet.getByRole('heading', { name: 'Меню сессии' }).tap();
  await expect(sheet).toBeVisible();
  await page.touchscreen.tap(195, 12);
  await expect(sheet).toHaveCount(0);
});

test('more_menu_of_the_list_closes_outside_and_keeps_its_toggle', async ({ page, isMobile }) => {
  await boot(page);
  const toggle = page.getByRole('button', { name: 'Ещё', exact: true });
  const menu = page.getByRole('menu', { name: 'Ещё' });
  const outside = (): Promise<void> => (isMobile ? page.locator('.brand').tap() : page.locator('.brand').click());
  await toggle.click();
  await expect(menu).toBeVisible();
  await outside();
  await expect(menu).toHaveCount(0);
  await toggle.click();
  await toggle.click();
  await expect(menu).toHaveCount(0);
  await toggle.click();
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
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
  await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
  await page.getByRole('dialog', { name: 'Переместить сессию в корзину?' }).getByRole('button', { name: 'В корзину', exact: true }).click();
  await expect(page).toHaveURL(/\/sessions$/);
  await expect(page.locator(`[data-session="${ids.plain}"]`)).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Вернуть' })).toBeVisible();
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
  await expect(page.locator(`[data-session="${ids.plain}"] .activity`)).toHaveText(/ · \d{2}:\d{2}$/);
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

test('limits_strip_warns_below_20_remaining_and_shows_the_reserve_variant', async ({ page }) => {
  const { hub, api, ids } = await boot(page);
  await expect(page.locator('.limits .pct[data-tone="danger"]')).toHaveText(/4%/);
  await expect(page.locator('.limits[data-tone="danger"]')).toBeVisible();
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
  await expect(strip).toContainText('Claude');
  await expect(strip.locator('.meter').first()).toContainText('5ч');
  await expect(strip).toContainText('нед');
  await expect(strip).toContainText('4%');
  await expect(strip).toContainText('Codex');
  await expect(strip.locator('.reserve')).toHaveText(['резерв']);
});

test('empty_states_offer_actions', async ({ page }) => {
  await boot(page, true);
  await expect(page.locator('code', { hasText: 'canterlot machine add' })).toBeVisible();
  await expect(page.getByRole('link', { name: /machine add/ })).toHaveCount(0);
  const create = page.getByRole('link', { name: 'Создать первую сессию' });
  await expect(create).toBeVisible();
  expect((await create.boundingBox())!.height).toBeGreaterThanOrEqual(44);
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

test('more_menu_reloads_the_page', async ({ page }) => {
  await boot(page);
  await page.evaluate(() => { (window as unknown as { marker: number }).marker = 1; });
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await Promise.all([
    page.waitForEvent('load'),
    page.getByRole('menuitem', { name: 'Обновить страницу' }).click()
  ]);
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  expect(await page.evaluate(() => (window as unknown as { marker?: number }).marker)).toBeUndefined();
});

test('row_dot_shows_the_state_while_the_owner_colour_waits_for_a_quiet_session', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const busy = seedConversation(api, { title: 'Синяя, но работает', color: 'blue', activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() } } as never);
  const stalled = seedConversation(api, { title: 'Серая, но подвисла', color: 'neutral', activity: { state: 'stalled', evidence: { last_event_at: new Date(Date.now() - 3_600_000).toISOString() }, since: new Date().toISOString() } } as never);
  const quiet = seedConversation(api, { title: 'Синяя и спокойная', color: 'blue', activity: { state: 'stopped', evidence: { outcome: 'stopped' }, since: new Date().toISOString() } } as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(`[data-session="${busy}"] .mark`)).toHaveAttribute('data-token', '--accent');
  await expect(page.locator(`[data-session="${stalled}"] .mark`)).toHaveAttribute('data-token', '--warning');
  await expect(page.locator(`[data-session="${quiet}"] .mark`)).toHaveAttribute('data-token', '--blue');
  // Длинное название, обрезанное в строке, читается целиком в подсказке.
  await expect(page.locator(`[data-session="${busy}"] .title`)).toHaveAttribute('title', 'Синяя, но работает');
});

test('search_focus_ring_wraps_the_whole_bar', async ({ page }) => {
  await boot(page);
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  const field = page.getByRole('textbox', { name: 'Поиск сессий' });
  await field.fill('поток');
  await field.focus();
  await expect(field).toHaveCSS('outline-style', 'none');
  await expect(page.locator('.search')).not.toHaveCSS('outline-style', 'none');
});

test('list_reserves_the_limits_strip_and_hides_zero_counts_until_the_snapshot', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  let release: () => void = () => undefined;
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route('**/api/v1/snapshot**', async (route) => { await held; await route.fallback(); });
  await page.goto('/sessions');
  // До снимка нули в счёте вкладок не выдают себя за пустой список, а место полосы лимитов занято.
  await expect(page.locator('.strip-slot')).toHaveCount(1);
  await expect(page.locator('.tabs .count')).toHaveCount(0);
  // Заглушка переезжает из ветки без runtime в ветку списка: меряем только утвердившийся узел.
  await expect(page.locator('.tabs')).toBeVisible();
  await expect(page.locator('.strip-slot')).toBeVisible();
  let slot = { x: 0, y: 0, width: 0, height: 0 };
  await expect.poll(async () => {
    const box = await page.locator('.strip-slot').boundingBox();
    if (!box) return false;
    slot = box;
    return true;
  }).toBe(true);
  release();
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.strip-slot')).toHaveCount(0);
  await expect(page.locator('.tabs .count').first()).toBeVisible();
  // Пришедшая полоса не выше оставленного места: строки под ней не уезжают вниз.
  const stripNode = page.locator('.limits').first();
  await expect(stripNode).toBeVisible();
  await expect.poll(async () => (await stripNode.boundingBox())?.height ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(slot.height + 1);
  await expect.poll(async () => {
    const box = await stripNode.boundingBox();
    return box ? Math.abs(box.y - slot.y) : Number.POSITIVE_INFINITY;
  }).toBeLessThanOrEqual(1);
});

test('list_bottom_edge_fades_instead_of_slicing_the_next_row', async ({ page }) => {
  await boot(page);
  const scroll = page.locator('[data-list-scroll]');
  // Строка под краем гаснет к навигации, а не стоит полоской верхушек букв.
  expect(await scroll.evaluate((node) => getComputedStyle(node).maskImage || getComputedStyle(node).webkitMaskImage)).toContain('linear-gradient');
});
