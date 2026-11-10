import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

const SPAWN = {
  harness: 'codex',
  model: 'gpt-5',
  effort: 'medium',
  title: null,
  workdir: '/work/app',
  isolated: true,
  base: null,
  preset: 'clean',
  permission_mode: 'ask',
  brief: 'Разобрать падающий тест и поправить его',
  subscribe: true,
  chain_depth: 0
};

async function boot(page: Page, child: { worktree?: { branch: string; state: string; path: string } | null } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, { title: 'Оркестратор', model: 'opus-5.5' });
  const id = seedConversation(api, { title: 'Миграция схемы', model: 'opus-5.5' });
  const detail = api.details.get(id)!;
  detail.parent_id = parent;
  detail.spawn_depth = 1;
  detail.worktree = child.worktree === undefined ? null : child.worktree;
  api.histories.set(id, [
    { kind: 'input', id: api.id(), idempotency_key: 'k-1', state: 'accepted', content: [{ type: 'text', text: 'Сделай миграцию' }], attachments: [], created_seq: '1', feed_seq: '1', created_at: new Date().toISOString(), origin: { kind: 'spawn', from: parent, title: 'Оркестратор', model: 'sonnet-5', harness: 'claude_code', chain_depth: 1 } },
    { kind: 'input', id: api.id(), idempotency_key: 'k-2', state: 'accepted', content: [{ type: 'text', text: 'Вопрос владельца' }], attachments: [], created_seq: '2', feed_seq: '2', created_at: new Date().toISOString() }
  ] as never);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id, parent };
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
}

test('approval_card_allow_and_deny', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  const { hub, api, id } = await boot(page);

  const first = api.id();
  hub.append(`conversation:${id}`, { kind: 'request_opened', conversation_id: id, payload: { request_id: first, kind: 'spawn_approval', payload: SPAWN } });
  hub.flush();

  const card = page.locator('[data-spawn-approval]');
  await expect(card.getByRole('heading', { name: 'Агент просит запустить сессию' })).toBeVisible();
  await expect(page.getByText('Вопрос от агента')).toHaveCount(0);
  for (const word of ['Харнесс', 'Модель', 'Режим', 'Каталог', 'Отдельная рабочая копия', 'Задача']) {
    await expect(card.getByText(word, { exact: true })).toBeVisible();
  }
  await expect(card.getByText('/work/app')).toBeVisible();
  await expect(card.getByText('Разобрать падающий тест и поправить его')).toBeVisible();
  await noOverflow(page);

  await card.getByRole('button', { name: 'Отклонить' }).focus();
  await expect(card.getByRole('button', { name: 'Отклонить' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect.poll(() => api.decisions.get(first)?.decision).toEqual({ kind: 'permission', behavior: 'deny' });

  const second = api.id();
  hub.append(`conversation:${id}`, { kind: 'request_opened', conversation_id: id, payload: { request_id: second, kind: 'spawn_approval', payload: SPAWN } });
  hub.flush();
  await page.locator('.request-cards-slot [data-action="allow"]').click();
  await expect.poll(() => api.decisions.get(second)?.decision).toEqual({ kind: 'permission', behavior: 'allow' });
});

test('parent_link_opens_parent', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  const { parent } = await boot(page);
  const bubble = page.locator('[data-role="session-message"][data-origin="spawn"]');
  await expect(bubble.getByText('Задание от сессии «Оркестратор»')).toBeVisible();
  await expect(bubble.getByText('sonnet-5')).toBeVisible();
  await expect(page.locator('article[data-origin]')).toHaveCount(1);
  await noOverflow(page);

  const link = page.getByRole('link', { name: 'Запущена сессией Оркестратор' });
  await expect(link).toBeVisible();
  // Ссылка держит зону нажатия 44px, а кнопки шапки стоят у названия, не посередине трёх строк.
  expect((await link.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  const title = (await page.locator('.chat-header .header-title').boundingBox())!;
  const menu = (await page.getByRole('button', { name: 'Меню сессии' }).boundingBox())!;
  expect(menu.y).toBeLessThanOrEqual(title.y + title.height);
  expect(menu.y + menu.height).toBeLessThan((await link.boundingBox())!.y + (await link.boundingBox())!.height / 2);
  await link.focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(new RegExp(`/c/${parent}$`));
  await expect(page.getByRole('heading', { name: 'Оркестратор' })).toBeVisible();
});

test('accept_flow', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  const { api, id } = await boot(page, { worktree: { branch: 'canterlot/0190abcd', state: 'active', path: '/data/worktrees/x' } });
  api.acceptFindsDirtyCopy = true;

  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Принять результат и убрать' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('Сессия остановится, рабочая копия будет удалена. Ветка с коммитами останется.')).toBeVisible();
  await dialog.getByRole('button', { name: 'Принять результат и убрать' }).click();

  await expect.poll(() => api.acceptCalls.length).toBe(1);
  expect(api.acceptCalls[0]).toMatchObject({ id, force: false });
  expect(api.acceptCalls[0]!.key).toBeTruthy();

  const notice = page.locator('[data-worktree-dirty]');
  await expect(notice.getByText('В рабочей копии есть незакоммиченные правки или коммиты вне веток')).toBeVisible();
  await noOverflow(page);
  await notice.getByRole('button', { name: 'Убрать всё равно' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Убрать всё равно' }).click();

  await expect.poll(() => api.acceptCalls.length).toBe(2);
  expect(api.acceptCalls[1]).toMatchObject({ id, force: true });
  expect(api.acceptCalls[1]!.key).not.toBe(api.acceptCalls[0]!.key);
  await expect(notice).toHaveCount(0);

  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await expect(page.getByRole('menuitem', { name: 'Принять результат и убрать' })).toHaveCount(0);
});
