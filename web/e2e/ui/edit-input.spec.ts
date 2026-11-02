import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

function acceptedInput(api: FakeHubApi, seq: string, text: string) {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `k-${seq}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: seq,
    feed_seq: seq,
    created_at: AT
  };
}

function queuedInput(api: FakeHubApi, seq: string, text: string) {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `k-${seq}`,
    state: 'queued',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: seq,
    created_at: AT
  };
}

async function boot(page: import('@playwright/test').Page, items: unknown[]) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Правка',
    harness: { id: 'codex', title: 'codex', provider: 'x' }
  } as never);
  api.histories.set(id, items as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

async function sendOwn(page: import('@playwright/test').Page, text: string) {
  let ownKey = '';
  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    ownKey = route.request().headers()['idempotency-key'] ?? '';
    return route.fallback();
  });
  await page.getByRole('textbox', { name: 'Сообщение' }).fill(text);
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  const bubble = page.locator('.bubble', { hasText: text });
  await expect(bubble).toBeVisible();
  return { bubble, ownKey };
}

function turnRunning(hub: FakeHub, id: string, runningInputId: string) {
  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 1, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: runningInputId } } }
  });
  hub.flush();
}


async function confirmQueued(
  page: import('@playwright/test').Page,
  hub: FakeHub,
  api: FakeHubApi,
  id: string,
  ownKey: string,
  text: string,
) {
  hub.append(`conversation:${id}`, {
    kind: 'input_queued',
    conversation_id: id,
    payload: {
      input_id: api.id(),
      input_seq: '1',
      idempotency_key: ownKey,
      state: 'queued',
      content: [{ type: 'text', text }]
    }
  });
  hub.flush();
  const bubble = page.locator('.bubble', { hasText: text });
  await expect(bubble.getByRole('button', { name: 'Изменить' })).toBeVisible();
  return bubble;
}

test('edit_queued_bubble_in_place_and_save', async ({ page }) => {
  const { hub, api, id } = await boot(page, []);
  const { ownKey } = await sendOwn(page, 'Черновик в очереди');
  expect(ownKey).not.toBe('');
  const bubble = await confirmQueued(page, hub, api, id, ownKey, 'Черновик в очереди');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') });
  await expect(editor.locator('textarea')).toBeVisible();
  await editor.locator('textarea').fill('Отредактировано');
  const hits: string[] = [];
  await page.route('**/api/v1/conversations/*/inputs/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    hits.push(route.request().url());
    await route.fulfill({ json: { data: { input_id: 'inp', revision: 2 } } });
  });
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect.poll(() => hits.length).toBe(1);
  const editedId = hits[0]!.split('/inputs/')[1]!;
  hub.append(`conversation:${id}`, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: {
      input_id: editedId,
      state: 'queued',
      attempt: 0,
      cause: 'edited',
      revision: 2,
      content: [{ type: 'text', text: 'Отредактировано' }]
    }
  });
  hub.flush();
  await expect(page.locator('.bubble', { hasText: 'Отредактировано' })).toBeVisible();
  await expect(page.getByText('Изменено').first()).toBeVisible();
});

test('edit_cancel_discards_changes', async ({ page }) => {
  const { hub, api, id } = await boot(page, []);
  const { bubble, ownKey } = await sendOwn(page, 'Не трогать');
  await confirmQueued(page, hub, api, id, ownKey, 'Не трогать');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') });
  await editor.locator('textarea').fill('Случайный текст');
  await editor.getByRole('button', { name: 'Отмена' }).click();
  await expect(bubble.getByRole('button', { name: 'Изменить' })).toBeVisible();
  await expect(page.locator('.bubble', { hasText: 'Случайный текст' })).toHaveCount(0);
});

test('edit_strip_item_while_turn_runs_and_remove_attachment', async ({ page }) => {
  const fileId = 'aaaaaaaa-0000-4000-8000-000000000001';
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Правка',
    harness: { id: 'codex', title: 'codex', provider: 'x' }
  } as never);
  api.histories.set(id, [
    acceptedInput(api, '1', 'В ход'),
    {
      ...queuedInput(api, '2', 'С файлом'),
      attachments: [
        {
          attachment_id: fileId,
          name: 'a.txt',
          mime: 'text/plain',
          size: 3,
          sha256: 'x',
          image: false
        }
      ],
      content: [
        { type: 'text', text: 'С файлом' },
        {
          type: 'attachment',
          attachment_id: fileId,
          name: 'a.txt',
          mime: 'text/plain',
          size: 3,
          sha256: 'x',
          image: false
        }
      ]
    }
  ] as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  turnRunning(hub, id, (api.histories.get(id) as { id: string }[])[0]!.id);
  const item = page.locator('.strip .item', { hasText: 'С файлом' });
  await expect(item.getByRole('button', { name: 'Изменить' })).toBeVisible();
  await item.getByRole('button', { name: 'Изменить' }).click();
  await item.getByRole('button', { name: /Убрать вложение/ }).click();
  const hits: { body: unknown }[] = [];
  await page.route('**/api/v1/conversations/*/inputs/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    hits.push({ body: route.request().postDataJSON() });
    await route.fulfill({ json: { data: { input_id: 'inp', revision: 2 } } });
  });
  await item.getByRole('button', { name: 'Сохранить' }).click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0]!.body).toMatchObject({ attachment_ids: [] });
});

test('edit_refusal_shows_reason_in_the_bubble', async ({ page }) => {
  const { hub, api, id } = await boot(page, []);
  const { bubble, ownKey } = await sendOwn(page, 'Уже ушло');
  await confirmQueued(page, hub, api, id, ownKey, 'Уже ушло');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  await page.route('**/api/v1/conversations/*/inputs/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    await route.fulfill({
      status: 400,
      json: {
        error: {
          code: 'invalid_request',
          message: 'Некорректный запрос',
          scope: 'browser',
          retry: 'never',
          correlation_id: '00000000-0000-4000-8000-0000000000f1',
          details: { reason: 'input_not_queued' }
        }
      }
    });
  });
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') });
  await editor.locator('textarea').fill('Поздно');
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.locator('.bubble', { hasText: 'Уже ушло' }).getByText('Сообщение уже ушло, править поздно')).toBeVisible();
  await expect(page.locator('.bubble').filter({ has: page.locator('textarea') })).toHaveCount(0);
});

test('edit_conflict_keeps_the_editor_open', async ({ page }) => {
  const { hub, api, id } = await boot(page, []);
  const { bubble, ownKey } = await sendOwn(page, 'Моё');
  await confirmQueued(page, hub, api, id, ownKey, 'Моё');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  await page.route('**/api/v1/conversations/*/inputs/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    await route.fulfill({
      status: 400,
      json: {
        error: {
          code: 'invalid_request',
          message: 'Некорректный запрос',
          scope: 'browser',
          retry: 'never',
          correlation_id: '00000000-0000-4000-8000-0000000000f1',
          details: { reason: 'revision_conflict' }
        }
      }
    });
  });
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') });
  await editor.locator('textarea').fill('Чужое');
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect(editor.getByText('Сообщение уже изменено в другом месте')).toBeVisible();
  await expect(editor.locator('textarea')).toBeVisible();
});
