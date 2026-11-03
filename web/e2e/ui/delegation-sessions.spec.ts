import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import type { ListRow } from '../../src/lib/stores/list.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

const WORKING = { state: 'working', evidence: { phase: 'generating' }, since: '2026-10-28T12:00:00.000Z' };
const IDLE = { state: 'idle', evidence: {}, since: '2026-10-28T12:00:00.000Z' };
const STOPPED = { state: 'stopped', evidence: { outcome: 'user_stop' }, since: '2026-10-28T12:00:00.000Z' };

function seedFamily(api: FakeHubApi) {
  const group = api.id();
  api.groups = [{ id: group, name: 'canterlot', position: 0, created_at: '2026-10-28T12:00:00.000Z' }];
  const machine = { id: api.id(), name: 'workstation' };
  const harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  const parent = seedConversation(api, {
    title: 'Оркестратор',
    groups: [group],
    machine,
    harness,
    activity: IDLE as never,
  });
  const child = seedConversation(api, {
    title: 'Миграция схемы',
    parent_id: parent,
    spawn_depth: 1,
    machine,
    harness,
    model: 'sonnet',
    working_ms: 3_900_000,
    activity: WORKING as never,
  });
  const done = seedConversation(api, {
    title: 'Разбор логов',
    parent_id: parent,
    spawn_depth: 1,
    machine,
    harness,
    activity: STOPPED as never,
  });
  const plain = seedConversation(api, {
    title: 'Обычная сессия',
    machine,
    harness,
    activity: IDLE as never,
  });
  api.counts = {
    all: 4,
    pinned: 0,
    archived: 0,
    groups: [{ id: group, count: 1 }],
  };
  return { group, parent, child, done, plain };
}

function derivedPayload(api: FakeHubApi, id: string, activity: unknown) {
  const row = api.list.get(id)!;
  return {
    conversation_id: id,
    activity,
    unread_count: 0,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
    last_activity_at: '2026-10-28T12:00:00.000Z',
    settings: { model: row.model, effort: 'medium', permission_mode: 'owner' },
    crunch: { enabled: false },
    working_ms: row.working_ms ?? 0,
  };
}

async function bootList(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedFamily(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

test('child_nests_under_parent_and_keeps_counts', async ({ page }) => {
  const { ids } = await bootList(page);
  // Разделители в строке-сгибе не слипаются с числом работающих.
  const fold = page.getByRole('button', { name: /делегированные: 2/ });
  await expect(fold).toBeVisible();
  await expect(fold).toContainText('Делегированные · 2 · работает 1');
  const child = page.locator(`[data-session="${ids.child}"]`);
  await expect(child).toBeVisible();
  await expect(child).toHaveClass(/depth-1/);
  // Ветку рисует строка-сгиб: в самой строке второго значка и склейки с названием нет.
  await expect(child.locator('.title')).not.toContainText('└');
  await expect(child.locator('.title')).toHaveText('Миграция схемы');
  await expect(child.locator('[data-worked]')).toHaveText('работала 1 ч 5 мин');
  // Ребёнок не занимает места в счёте группы и вкладок.
  await expect(page.getByRole('tab', { name: /canterlot 1/ })).toBeVisible();
  await expect(page.getByRole('tab', { name: /Все 4/ })).toBeVisible();
  // Во вкладке группы ребёнок едет с родителем, а не своей строкой.
  await page.getByRole('tab', { name: /canterlot/ }).click();
  await expect(page.locator(`[data-session="${ids.child}"]`)).toHaveClass(/depth-1/);
  await expect(page.getByRole('tab', { name: /canterlot 1/ })).toBeVisible();
});

test('child_status_dot_uses_the_same_activity_colour_as_a_top_level_row', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, { title: 'Оркестратор', activity: IDLE as never });
  const child = seedConversation(api, {
    title: 'Выкатка ba35992',
    parent_id: parent,
    spawn_depth: 1,
    activity: WORKING as never
  });
  const top = seedConversation(api, { title: 'Обычная работа', activity: WORKING as never });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const childMark = page.locator(`[data-session="${child}"] .mark`);
  const topMark = page.locator(`[data-session="${top}"] .mark`);
  await expect(childMark).toHaveAttribute('data-token', '--accent');
  expect(await childMark.getAttribute('data-token')).toBe(await topMark.getAttribute('data-token'));
  // Спокойная строка без выбранного цвета остаётся нейтральной.
  await expect(page.locator(`[data-session="${parent}"] .mark`)).toHaveAttribute('data-token', '--muted');
});

test('grandchild_keeps_indentation_without_a_second_tree_mark', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const root = seedConversation(api, { title: 'Оркестратор', activity: IDLE as never });
  const child = seedConversation(api, {
    title: 'Миграция схемы',
    parent_id: root,
    spawn_depth: 1,
    activity: IDLE as never
  });
  const grandchild = seedConversation(api, {
    title: 'Проверка',
    parent_id: child,
    spawn_depth: 2,
    activity: WORKING as never
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const childRow = page.locator(`[data-session="${child}"]`);
  const grandchildRow = page.locator(`[data-session="${grandchild}"]`);
  await expect(childRow).toHaveClass(/depth-1/);
  await expect(grandchildRow).toHaveClass(/depth-2/);
  await expect(grandchildRow.locator('.title')).toHaveText('Проверка');
  await expect(grandchildRow.locator('.title')).not.toContainText('└');
  const childBox = await childRow.boundingBox();
  const grandchildBox = await grandchildRow.boundingBox();
  expect(grandchildBox!.x).toBeGreaterThan(childBox!.x);
});

test('kids_toggle_collapses_and_remembers_choice', async ({ page }) => {
  const { ids } = await bootList(page);
  // Живой ребёнок раскрыт по умолчанию, finished закрыт выбором ниже.
  await expect(page.locator(`[data-session="${ids.child}"]`)).toBeVisible();
  const toggle = page.getByRole('button', { name: /делегированные: 2/ });
  await toggle.click();
  await expect(page.locator(`[data-session="${ids.child}"]`)).toHaveCount(0);
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await page.reload();
  await expect(page.locator('[data-list-ready]')).toBeVisible();
  await expect(page.locator(`[data-session="${ids.child}"]`)).toHaveCount(0);
  await page.getByRole('button', { name: /делегированные: 2/ }).click();
  await expect(page.locator(`[data-session="${ids.child}"]`)).toBeVisible();
});

test('finished_child_starts_closed', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, { title: 'Оркестратор', activity: IDLE as never });
  const done = seedConversation(api, {
    title: 'Разбор логов',
    parent_id: parent,
    spawn_depth: 1,
    activity: STOPPED as never,
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(`[data-session="${done}"]`)).toHaveCount(0);
  const toggle = page.getByRole('button', { name: 'Развернуть делегированные: 1' });
  await expect(toggle).toBeVisible();
  await expect(toggle).not.toContainText('работает');
  await toggle.click();
  await expect(page.locator(`[data-session="${done}"]`)).toBeVisible();
});

test('orphan_archive_and_search', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const group = api.id();
  api.groups = [{ id: group, name: 'canterlot', position: 0, created_at: '2026-10-28T12:00:00.000Z' }];
  const machine = { id: api.id(), name: 'workstation' };
  const harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  const parent = seedConversation(api, { title: 'Оркестратор', machine, harness, activity: IDLE as never });
  const child = seedConversation(api, {
    title: 'Миграция схемы',
    parent_id: parent,
    spawn_depth: 1,
    groups: [group],
    machine,
    harness,
    activity: WORKING as never,
  });
  api.counts = { all: 2, pinned: 0, archived: 0, groups: [{ id: group, count: 1 }] };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });

  // В общей ленте ребёнок под родителем, в группе без него — обычной строкой.
  await expect(page.locator(`[data-session="${child}"]`)).toHaveClass(/depth-1/);
  await page.getByRole('tab', { name: /canterlot/ }).click();
  const orphan = page.locator(`[data-session="${child}"]`);
  await expect(orphan).toBeVisible();
  await expect(orphan).not.toHaveClass(/depth-1/);
  await expect(page.getByRole('button', { name: /делегированные/ })).toHaveCount(0);
  await page.getByRole('tab', { name: /Все/ }).click();

  // Удалённый родитель оставляет ребёнка обычной строкой и в общей ленте.
  api.deleteConversation(parent);
  const alone = page.locator(`[data-session="${child}"]`);
  await expect(alone).toBeVisible();
  await expect(alone).not.toHaveClass(/depth-1/);

  // Архивный ребёнок идёт в архив обычной строкой: родитель не в архиве.
  const row = api.list.get(child)!;
  row.archived = true;
  api.counts = {
    all: 1,
    pinned: 0,
    archived: 1,
    groups: api.groups.map((item) => ({ id: item.id, count: 0 })),
  };
  hub.append('conversations', {
    kind: 'conversation_updated',
    conversation_id: child,
    payload: {
      id: child,
      title: row.title,
      title_source: 'user',
      color: null,
      pinned: false,
      archived: true,
      groups: row.groups,
      machine: row.machine,
      harness: row.harness,
      workdir: row.workdir,
      created_at: row.created_at,
      counts: api.counts,
    },
  });
  hub.flush();
  await expect(page.locator(`[data-session="${child}"]`)).toHaveCount(0);
  await page.getByRole('tab', { name: /Архив/ }).click();
  const archived = page.locator(`[data-session="${child}"]`);
  await expect(archived).toBeVisible();
  await expect(archived).not.toHaveClass(/depth-1/);

  // Поиск находит ребёнка.
  await page.getByRole('tab', { name: /Все/ }).click();
  await page.getByRole('button', { name: 'Поиск', exact: true }).click();
  await page.getByRole('textbox', { name: 'Поиск сессий' }).fill('Миграция');
  await expect(page.locator(`[data-session="${child}"]`)).toBeVisible();
});

test('long_titles_keep_working_at_384px', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, {
    title: 'Оркестратор с очень длинным названием сессии, которое не помещается в строку',
    activity: IDLE as never,
  });
  seedConversation(api, {
    title: 'Миграция схемы с очень длинным названием делегированной сессии для проверки переноса',
    parent_id: parent,
    spawn_depth: 1,
    activity: WORKING as never,
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.setViewportSize({ width: 384, height: 800 });
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

async function bootChat(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, { title: 'Оркестратор', activity: IDLE as never });
  const child = seedConversation(api, {
    title: 'Миграция схемы',
    parent_id: parent,
    spawn_depth: 1,
    model: 'sonnet',
    working_ms: 3_900_000,
    activity: WORKING as never,
  });
  const childDetail = api.details.get(child) as Record<string, unknown>;
  childDetail.parent_id = parent;
  childDetail.spawn_depth = 1;
  api.bindConversation(parent);
  api.bindConversation(child);
  await installChatRoutes(page, hub, api, OWNER);
  return { hub, api, parent, child };
}

function sessionLink(page: Page, id: string) {
  return page.locator(`a.tree-node[href$="/c/${id}"]`);
}

test('phone_sheet_lists_child_sessions_and_opens_them', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'лист делегирования — телефонный');
  const { parent, child } = await bootChat(page);
  await page.goto(`/c/${parent}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const entry = page.getByRole('button', { name: 'Делегирование, работает 1' });
  await expect(entry).toBeVisible();
  await entry.click();
  const sheet = page.getByRole('dialog', { name: 'Делегирование' });
  await expect(sheet).toBeVisible();
  await expect(sheet.getByText('Сессии')).toBeVisible();
  const link = sessionLink(sheet, child);
  await expect(link).toContainText('Миграция схемы');
  await expect(link.locator('.node-task')).toHaveText('sonnet · работала 1 ч 5 мин');
  await expect(link).toContainText('Генерирует');
  await page.screenshot({ path: `test-results/delegation-session-worked-${test.info().project.name}.png` });
  await link.click();
  await expect(page).toHaveURL(new RegExp(`/c/${child}$`));
});

test('child_state_updates_live_and_header_count_follows', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'счётчик входа — телефонный');
  const { hub, api, parent, child } = await bootChat(page);
  await page.goto(`/c/${parent}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('button', { name: 'Делегирование, работает 1' })).toBeVisible();

  const row = api.list.get(child) as ListRow;
  row.activity = STOPPED as never;
  hub.append('conversations', {
    kind: 'derived_changed',
    conversation_id: child,
    payload: derivedPayload(api, child, STOPPED),
  });
  hub.flush();
  await expect(page.getByRole('button', { name: 'Делегирование' })).toBeVisible();
  await page.getByRole('button', { name: 'Делегирование' }).click();
  const sheet = page.getByRole('dialog', { name: 'Делегирование' });
  await expect(sheet.getByText('Завершённые · 1')).toBeVisible();
  await sheet.getByText('Завершённые · 1').click();
  await expect(sessionLink(sheet, child)).toContainText('Остановлена вами');
});

test('desktop_panel_lists_child_sessions', async ({ page, isMobile }) => {
  test.skip(isMobile, 'панель делегирования — настольная');
  const { parent, child } = await bootChat(page);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`/c/${parent}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: 'Делегирование' }).click();
  const panel = page.locator('[data-pane="lane"]');
  await expect(panel).toBeVisible();
  await expect(panel.getByText('Сессии')).toBeVisible();
  const link = sessionLink(panel, child);
  await expect(link).toContainText('Миграция схемы');
  await expect(link.locator('.node-task')).toHaveText('sonnet · работала 1 ч 5 мин');
  await link.click();
  await expect(page).toHaveURL(new RegExp(`/c/${child}$`));
});
