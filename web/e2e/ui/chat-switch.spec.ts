import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function bootPair(page: Page) {
  // На телефоне список сессий рядом с чатом не показывается: смену сессии на месте даёт планшетная ширина.
  if ((page.viewportSize()?.width ?? 0) < 704) await page.setViewportSize({ width: 900, height: 900 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const a = seedConversation(api, { title: 'Сессия Альфа' });
  const b = seedConversation(api, { title: 'Сессия Бета' });
  const [first] = seedHistory(api, a, 1);
  const [second] = seedHistory(api, b, 1);
  first!.content = 'ответ из альфы';
  first!.summary = 'задача альфы';
  second!.content = 'ответ из беты';
  second!.summary = 'задача беты';
  await installChatRoutes(page, hub, api, OWNER);
  return { hub, api, a, b, first: first!, second: second! };
}

function inputPosts(page: Page) {
  const urls: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && /\/api\/v1\/conversations\/[^/]+\/inputs$/.test(new URL(request.url()).pathname))
      urls.push(new URL(request.url()).pathname);
  });
  return urls;
}

test('sidebar_switch_rebinds_timeline_header_and_composer_to_the_new_session', async ({ page }) => {
  const { a, b } = await bootPair(page);
  const posts = inputPosts(page);
  await page.goto(`/c/${a}`);
  const chat = page.locator('[data-pane="chat"]');
  await expect(chat.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(chat.getByRole('heading', { level: 1 })).toHaveText('Сессия Альфа');
  await expect(chat.getByText('ответ из альфы')).toBeVisible();
  const area = chat.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('черновик для альфы');

  await page.locator('[data-pane="list"]').getByRole('link', { name: /Сессия Бета/ }).click();
  await expect(page).toHaveURL(new RegExp(`/c/${b}$`));
  await expect(chat.getByRole('heading', { level: 1 })).toHaveText('Сессия Бета');
  await expect(chat.getByText('ответ из беты')).toBeVisible();
  await expect(chat.getByText('ответ из альфы')).toHaveCount(0);
  await expect(area).toHaveValue('');

  await area.fill('сообщение для беты');
  await chat.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => posts.length).toBe(1);
  expect(posts).toEqual([`/api/v1/conversations/${b}/inputs`]);
});

test('lane_route_follows_the_conversation_id_on_same_route_navigation', async ({ page }) => {
  const { a, b, first, second } = await bootPair(page);
  await page.goto(`/c/${a}/lane/${first.id}`);
  const lane = page.locator('.lane-screen[data-ready]');
  await expect(lane).toBeVisible({ timeout: 15_000 });
  await expect(lane.getByRole('heading', { level: 1 })).toHaveText('задача альфы');
  await expect(lane.getByRole('link', { name: 'К оркестратору' })).toHaveAttribute('href', `/c/${a}`);

  // Ссылка внутри приложения: роутер SvelteKit переводит на тот же маршрут без нового монтирования.
  await page.locator('#main').evaluate((main, href) => {
    const link = document.createElement('a');
    link.href = href;
    link.textContent = 'к бете';
    main.append(link);
  }, `/c/${b}/lane/${second.id}`);
  await page.getByRole('link', { name: 'к бете' }).click();
  await expect(page).toHaveURL(new RegExp(`/c/${b}/lane/${second.id}$`));
  await expect(lane.getByRole('heading', { level: 1 })).toHaveText('задача беты');
  await expect(lane.getByRole('link', { name: 'К оркестратору' })).toHaveAttribute('href', `/c/${b}`);
});

test('waiting_activity_without_a_request_shows_a_notice_not_an_answer_form', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Ждёт ответа',
    activity: { state: 'waiting_for_user', evidence: { question: 'Какой профиль выбрать?' }, since: '2026-10-28T12:00:00.000Z' }
  });
  seedHistory(api, id, 1);
  await installChatRoutes(page, hub, api, OWNER);
  const decisions: string[] = [];
  page.on('request', (request) => { if (request.url().includes('/decision')) decisions.push(request.url()); });
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const notice = page.getByRole('region', { name: 'Агент ждёт ответа' });
  await expect(notice).toBeVisible();
  await expect(notice).toContainText('Какой профиль выбрать?');
  await expect(page.getByRole('button', { name: 'Ответить' })).toHaveCount(0);
  expect(decisions).toEqual([]);
});

test('elapsed_time_of_a_running_answer_keeps_counting', async ({ page }) => {
  const { hub, a } = await bootPair(page);
  await page.goto(`/c/${a}`);
  await expect(page.locator('[data-pane="chat"] [data-ready]')).toBeVisible({ timeout: 15_000 });
  const blockId = '00000000-0000-4000-8000-00000000f001';
  hub.append(`conversation:${a}`, {
    kind: 'block_started',
    conversation_id: a,
    payload: { block_id: blockId, rev: 0, kind: 'text', input_command_id: null, parent_block_id: null, agent: 'agent', model: 'opus', tool_name: null, summary: null, approved_by: null }
  });
  hub.append(`conversation:${a}`, { kind: 'block_delta', conversation_id: a, payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'идёт ответ' } });
  hub.flush();
  await expect(page.getByText('идёт ответ')).toBeVisible();
  await expect(page.getByText(/Генерирует · [2-9] с/)).toBeVisible({ timeout: 6_000 });
});
