import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d7';

async function openChat(page: import('@playwright/test').Page, hub: FakeHub, api: FakeHubApi, baseURL: string, parent: string) {
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`${baseURL}/c/${parent}`);
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible({ timeout: 15_000 });
}

function parentWithSide(api: FakeHubApi, title = 'Родитель') {
  const id = seedConversation(api, { title });
  api.details.set(id, {
    ...(api.details.get(id) as Record<string, unknown>),
    turn_recorded: true,
    side_of: null,
    execution: null
  });
  return id;
}

test('side_sheet_opens_asks_restarts_and_closes', async ({ page }, testInfo) => {
  test.setTimeout(120_000);
  const baseURL = testInfo.project.use.baseURL as string;
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = parentWithSide(api);
  await openChat(page, hub, api, baseURL, parent);

  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Спросить в стороне' }).click();

  const title = page.getByRole('heading', { name: /в стороне/ });
  await expect(title).toBeVisible({ timeout: 15_000 });
  const composer = page.getByRole('textbox', { name: 'Сообщение' }).last();
  await expect(composer).toBeVisible({ timeout: 15_000 });

  const sideId = api.sideChats.get(parent);
  expect(sideId).toBeTruthy();
  const sent = page.waitForRequest(
    (request) =>
      request.method() === 'POST' &&
      request.url().endsWith(`/api/v1/conversations/${sideId}/inputs`)
  );
  await composer.fill('почему тест упал?');
  await page.getByRole('button', { name: 'Отправить', exact: true }).last().click();
  const request = await sent;
  expect(request.postDataJSON().content[0].text).toBe('почему тест упал?');

  const deleted = page.waitForRequest(
    (r) =>
      r.method() === 'DELETE' &&
      r.url().endsWith(`/api/v1/conversations/${parent}/side`)
  );
  await page.getByRole('button', { name: 'Начать заново' }).click();
  await deleted;
  await expect(page.getByRole('heading', { name: /в стороне/ })).toBeVisible({ timeout: 15_000 });

  const closeLabel =
    testInfo.project.name === 'desktop' ? 'Закрыть боковой чат' : 'Закрыть';
  await page.getByRole('button', { name: closeLabel, exact: true }).click();
  await expect(page.getByRole('heading', { name: /в стороне/ })).toHaveCount(0);
});

test('side_slash_command_opens_and_sends', async ({ page }, testInfo) => {
  test.setTimeout(120_000);
  const baseURL = testInfo.project.use.baseURL as string;
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = parentWithSide(api, 'Команда');
  await openChat(page, hub, api, baseURL, parent);

  const composer = page.getByRole('textbox', { name: 'Сообщение' }).first();
  const created = page.waitForRequest(
    (r) =>
      r.method() === 'POST' &&
      r.url().endsWith(`/api/v1/conversations/${parent}/side`)
  );
  const sent = page.waitForRequest(
    (r) => r.method() === 'POST' && /\/conversations\/[^/]+\/inputs$/.test(r.url())
  );
  await composer.fill('/side что тут происходит?');
  await page.getByRole('button', { name: 'Отправить', exact: true }).first().click();
  await created;
  await expect(page.getByRole('heading', { name: /в стороне/ })).toBeVisible({ timeout: 15_000 });
  expect((await sent).postDataJSON().content[0].text).toBe('что тут происходит?');
});

test('opencode_side_entry_is_disabled_while_parent_runs', async ({ page }, testInfo) => {
  const baseURL = testInfo.project.use.baseURL as string;
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = parentWithSide(api, 'OpenCode');
  const row = api.list.get(parent)!;
  row.harness = { id: 'opencode', title: 'OpenCode', provider: 'opencode' };
  row.activity = {
    state: 'working',
    evidence: { kind: 'working', phase: 'tools' },
    since: new Date().toISOString()
  };
  api.details.set(parent, {
    ...(api.details.get(parent) as Record<string, unknown>),
    harness: row.harness,
    activity: row.activity
  });
  await openChat(page, hub, api, baseURL, parent);
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  const item = page.getByRole('menuitem', { name: 'Спросить в стороне' });
  await expect(item).toBeVisible();
  await expect(item).toBeDisabled();
  await expect(item).toHaveAttribute('title', 'Доступно, когда сессия остановится');
});
