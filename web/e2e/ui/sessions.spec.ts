import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions, type MockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, fillContrast, resizeViewport, test } from './fixtures';

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
  // Харнесс и машина ушли из строки: их называет подсказка модели во второй строке.
  await expect(page.locator('.who[title^="Claude Code · opus-5.5 · workstation"]').first()).toBeAttached();
  await expect(page.getByText('Генерирует').first()).toBeVisible();
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Светлая тема' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.getByRole('link', { name: /Восстановление потока/ })).toBeVisible();
});

test('rows_show_the_state_by_ring_dot_or_grey_and_the_owner_colour_by_a_stripe', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  const machine = { id: '00000000-0000-4000-8000-000000000101', name: 'workstation' };
  const harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  const idle = (title: string, alive?: boolean) => seedConversation(api, {
    title, harness, machine, model: 'opus', preview: 'Готово, тесты зелёные', last_activity_at: '2026-10-28T19:00:00.000Z',
    activity: { state: 'idle', evidence: { last_turn_at: '2026-10-28T19:00:00.000Z', ...(alive === undefined ? {} : { process_alive: alive }) }, since: '2026-10-28T19:00:00.000Z' }
  });
  idle('Живой простой', true);
  idle('Простой без процесса', false);
  idle('Простой со старого хаба');
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const row = (title: string) => page.locator('[data-session]').filter({ has: page.getByRole('link', { name: new RegExp(title) }) }).first();
  const avatar = (title: string) => row(title).locator('.avatar');
  await expect(row('Восстановление потока')).toHaveAttribute('data-state', 'running');
  await expect(avatar('Восстановление потока')).toHaveAttribute('data-halo', '--accent');
  await expect(row('Граница snapshot')).toHaveAttribute('data-state', 'waiting');
  await expect(avatar('Граница snapshot')).toHaveAttribute('data-halo', '--warning');
  // «Ждёт ответа» — самое важное состояние: слово жирное.
  expect(Number(await row('Граница snapshot').locator('.activity .text').evaluate((node) => getComputedStyle(node).fontWeight))).toBeGreaterThanOrEqual(700);
  await expect(row('Восстановление воркера')).toHaveAttribute('data-state', 'paused');
  await expect(avatar('Восстановление воркера')).toHaveClass(/dashed/);
  await expect(row('Сборка адаптера')).toHaveAttribute('data-state', 'stalled');
  await expect(avatar('Сборка адаптера')).toHaveAttribute('data-halo', '--danger');
  await expect(row('Повторная доставка ввода')).toHaveAttribute('data-state', 'stopped');
  await expect(avatar('Повторная доставка ввода')).toHaveClass(/dim/);
  // Простой: точная точка от хаба, серый без процесса, без поля — по-старому, как живой.
  await expect(row('Живой простой')).toHaveAttribute('data-state', 'live');
  await expect(avatar('Живой простой').locator('.dot')).toHaveAttribute('data-token', '--success');
  await expect(row('Живой простой')).toContainText('Готово, тесты зелёные');
  await expect(row('Простой без процесса')).toHaveAttribute('data-state', 'dormant');
  await expect(avatar('Простой без процесса')).toHaveClass(/dim/);
  await expect(row('Простой без процесса')).toContainText('Процесс не запущен');
  await expect(row('Простой со старого хаба')).toHaveAttribute('data-state', 'live');
  // Цвет владельца — полоска у края строки, знак состояния им не красится.
  await expect(row('Восстановление потока').locator('[data-owner-color="accent"]')).toBeAttached();
  await expect(row('Живой простой').locator('[data-owner-color]')).toHaveCount(0);
  // Знаки состояния не меняют высоту строки.
  const heights = await page.locator('[data-session]').evaluateAll((rows) => rows.filter((node) => !node.classList.contains('child')).map((node) => Math.round(node.getBoundingClientRect().height)));
  expect(new Set(heights).size).toBe(1);
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

test('selected_group_tab_stands_out_in_both_themes', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'на столе группы в полосе: выбор полосы проверяет session-chat-rows');
  await boot(page);
  const selected = page.getByRole('tablist', { name: 'Группы сессий' }).getByRole('tab', { selected: true });
  // Признак выбора — не текст, поэтому ему нужно 3:1 с фоном под ним (WCAG 1.4.11).
  expect(await fillContrast(selected)).toBeGreaterThanOrEqual(3);
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Светлая тема' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  expect(await fillContrast(selected)).toBeGreaterThanOrEqual(3);
});

test('phone_tabs_fade_only_the_edge_with_more_tabs_and_keep_the_selected_tab_clear', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'на рабочем столе вкладки переносятся, а не листаются');
  await boot(page);
  const tablist = page.getByRole('tablist', { name: 'Группы сессий' });
  await expect(tablist).toHaveAttribute('data-fade-end', '');
  await expect(tablist).not.toHaveAttribute('data-fade-start', '');
  // Пустые «Архив» и «Корзина» скрыты, последний видимый чип — bookshelf.
  await tablist.getByRole('tab').last().click();
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

// Фильтры владельца: пустые «Закреплённые» и «Личные проекты» не показываются.
async function bootOwnerFilters(page: Page, egorchik = 2) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  const ids = [api.id(), api.id(), api.id(), api.id()];
  api.groups = ['Canterlot', 'Работа', 'Егорчик', 'Личные проекты'].map((name, position) => ({ id: ids[position]!, name, position, created_at: '2026-10-28T12:00:00.000Z' }));
  const recount = (count: number) => {
    api.counts = { all: 5, pinned: 0, archived: 5, trashed: 3, groups: [{ id: ids[0]!, count: 1 }, { id: ids[1]!, count: 1 }, { id: ids[2]!, count }] };
  };
  recount(egorchik);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { recount, tablist: page.getByRole('tablist', { name: 'Группы сессий' }) };
}

/** Нажатие мимо меню: на столе — по пустому месту ленты, в стороне от списка. */
async function outsideList(page: Page, button: 'left' | 'right' = 'left'): Promise<void> {
  await page.locator('[data-pane="chat"]').click({ button, position: { x: 40, y: 320 } });
}

const tabNames = (tablist: Locator) => tablist.getByRole('tab').evaluateAll((tabs) => tabs.map((tab) => tab.textContent!.replace(/\s+/g, ' ').trim()));

test('desktop_group_chips_are_compact_and_fit_two_rows', async ({ page, isMobile }) => {
  test.skip(isMobile, 'на телефоне чипы остаются под палец');
  // Стол шире 74rem держит группы в полосе слева; чипы в панели списка остаются у планшета.
  await page.setViewportSize({ width: 1100, height: 900 });
  const { tablist } = await bootOwnerFilters(page);
  const boxes = await tablist.getByRole('tab').evaluateAll((tabs) => tabs.map((tab) => tab.getBoundingClientRect()).map((box) => ({ height: box.height, top: Math.round(box.top) })));
  expect(Math.max(...boxes.map((box) => box.height))).toBeLessThan(34);
  expect(new Set(boxes.map((box) => box.top)).size).toBeLessThanOrEqual(2);
  expect((await tablist.boundingBox())!.height).toBeLessThan(80);
});

test('empty_filters_hide_but_all_and_the_selected_one_stay', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'полоса стола показывает все фильтры: её проверяет session-chat-rows');
  const { tablist, recount } = await bootOwnerFilters(page);
  expect(await tabNames(tablist)).toEqual(['Все 5', 'Canterlot 1', 'Работа 1', 'Егорчик 2', 'Архив 5', 'Корзина 3']);
  // Стрелки ходят только по видимым чипам.
  const all = tablist.getByRole('tab', { name: /Все/ });
  await all.focus();
  await all.press('ArrowRight');
  await expect(tablist.getByRole('tab', { name: /Canterlot/ })).toHaveAttribute('aria-selected', 'true');
  await tablist.getByRole('tab', { name: /Canterlot/ }).press('ArrowLeft');
  await tablist.getByRole('tab', { name: /Все/ }).press('ArrowLeft');
  await expect(tablist.getByRole('tab', { name: /Корзина/ })).toHaveAttribute('aria-selected', 'true');

  await tablist.getByRole('tab', { name: /Егорчик/ }).click();
  recount(0);
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(tablist.getByRole('tab', { name: /Егорчик 0/ })).toHaveAttribute('aria-selected', 'true');
  await tablist.getByRole('tab', { name: /Все/ }).click();
  await expect(tablist.getByRole('tab', { name: /Егорчик/ })).toHaveCount(0);
});

test('phone_group_chips_keep_the_touch_target_in_one_scrolling_row', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'проверка пальцевой полосы');
  const { tablist } = await bootOwnerFilters(page);
  const boxes = await tablist.getByRole('tab').evaluateAll((tabs) => tabs.map((tab) => tab.getBoundingClientRect()).map((box) => ({ height: box.height, top: Math.round(box.top) })));
  expect(Math.min(...boxes.map((box) => box.height))).toBeGreaterThanOrEqual(44);
  expect(new Set(boxes.map((box) => box.top)).size).toBe(1);
  expect(await tablist.evaluate((node) => getComputedStyle(node).overflowX)).toBe('auto');
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
  await expect(page.locator(`[data-session="${ids.plain}"] [data-owner-color]`)).toHaveAttribute('data-owner-color', 'blue');
});

test('group_manager_create_rename_reorder_delete_keeps_sessions', async ({ page }) => {
  await boot(page);
  await page.getByRole('button', { name: 'Ещё' }).click();
  await page.getByRole('menuitem', { name: 'Группы' }).click();
  await page.getByRole('textbox', { name: 'Название группы' }).fill('черновики');
  await page.getByRole('button', { name: 'Создать' }).click();
  // Пустая группа не занимает чип на телефоне, но видна в управлении группами; полоса стола показывает и её.
  const railed = (await page.locator('[data-group-rail]').count()) > 0;
  await expect(page.getByRole('tab', { name: /черновики/ })).toHaveCount(railed ? 1 : 0);
  const renamed = page.getByRole('textbox', { name: 'Название черновики' });
  await renamed.fill('заметки');
  await renamed.press('Enter');
  await expect(page.getByRole('textbox', { name: 'Название заметки' })).toBeVisible();
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

test('archived_session_menu_offers_unarchive_and_moves_it_back_to_all', async ({ page }) => {
  const { api, ids } = await boot(page);
  const archived = seedConversation(api, {
    title: 'Архивная сессия',
    archived: true,
    machine: { id: ids.machine, name: 'workstation' },
    harness: { id: 'codex', title: 'Codex', provider: 'codex' }
  });
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });

  // Архивный список: меню предлагает вернуть сессию, а не отправить в архив повторно.
  await page.getByRole('tab', { name: /Архив/ }).click();
  const bodies: { archived?: boolean }[] = [];
  const keys: string[] = [];
  page.on('request', (request) => {
    if (request.method() !== 'PATCH' || !request.url().includes(archived)) return;
    keys.push(request.headers()['idempotency-key'] ?? '');
    bodies.push(request.postDataJSON() as { archived?: boolean });
  });
  await page.getByRole('button', { name: 'Действия: Архивная сессия' }).click();
  const unarchive = page.getByRole('button', { name: /Вернуть из архива/ });
  await expect(unarchive).toBeVisible();
  await expect(unarchive).toContainText('Сессия вернётся в список');
  await expect(page.getByRole('button', { name: /В архив/ })).toHaveCount(0);
  await unarchive.click();

  await expect.poll(() => bodies.length).toBe(1);
  expect(bodies[0]).toEqual({ archived: false });
  expect(keys[0]).not.toBe('');

  // Строка уходит из архива и появляется во «Все».
  await expect(page.locator(`[data-session="${archived}"]`)).toHaveCount(0);
  await page.getByRole('tab', { name: /Все/ }).click();
  await expect(page.locator(`[data-session="${archived}"]`)).toBeVisible();

  // Активная сессия по-прежнему предлагает отправить в архив.
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  await expect(page.getByRole('button', { name: /В архив/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Вернуть из архива/ })).toHaveCount(0);
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
  for (const [width, height] of [[1707, 960], [388, 760], [388, 587]] as const) {
    await resizeViewport(page, { width, height });
    for (const action of [actions.first(), actions.last()]) {
      await action.scrollIntoViewIfNeeded();
      await action.click();
      await expect(page.locator('.popover')).toBeVisible();
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
      await expect(page.locator('.popover')).toHaveCount(0);
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
  await outsideList(page);
  await expect(menu).toHaveCount(0);
  await row.getByRole('button').click();
  await expect(menu).toBeVisible();
  await outsideList(page, 'right');
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
    else await outsideList(page);
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
  const outside = (): Promise<void> => (isMobile ? page.locator('.brand').tap() : outsideList(page));
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
  // Время остаётся справа от названия и при непрочитанном.
  await expect(page.locator(`[data-session="${ids.plain}"] .l1 time`)).toHaveText(/^\d{2}:\d{2}$/);
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
  await expect(page.getByRole('button', { name: 'Избранное' })).toBeFocused();
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

test('row_ring_shows_the_state_and_the_stripe_keeps_the_owner_colour', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const busy = seedConversation(api, { title: 'Синяя, но работает', color: 'blue', activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() } } as never);
  const stalled = seedConversation(api, { title: 'Серая, но подвисла', color: 'neutral', activity: { state: 'stalled', evidence: { last_event_at: new Date(Date.now() - 3_600_000).toISOString() }, since: new Date().toISOString() } } as never);
  const quiet = seedConversation(api, { title: 'Синяя и спокойная', color: 'blue', activity: { state: 'stopped', evidence: { outcome: 'stopped' }, since: new Date().toISOString() } } as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  // Кольцо и точка говорят только о состоянии; цвет владельца — полоска у края строки при любом состоянии.
  await expect(page.locator(`[data-session="${busy}"] .avatar`)).toHaveAttribute('data-halo', '--accent');
  await expect(page.locator(`[data-session="${stalled}"] .avatar`)).toHaveAttribute('data-halo', '--danger');
  await expect(page.locator(`[data-session="${quiet}"] .avatar`)).not.toHaveAttribute('data-halo', /./);
  await expect(page.locator(`[data-session="${quiet}"] .avatar .dot`)).toHaveCount(0);
  for (const [id, color] of [[busy, 'blue'], [stalled, 'neutral'], [quiet, 'blue']] as const) {
    await expect(page.locator(`[data-session="${id}"] [data-owner-color]`)).toHaveAttribute('data-owner-color', color);
  }
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
