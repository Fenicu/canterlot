import type { Locator } from '@playwright/test';
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
  const inputId = api.id();
  hub.append(`conversation:${id}`, {
    kind: 'input_queued',
    conversation_id: id,
    payload: {
      input_id: inputId,
      input_seq: '1',
      idempotency_key: ownKey,
      state: 'queued',
      content: [{ type: 'text', text }]
    }
  });
  hub.flush();
  const bubble = page.locator('.bubble', { hasText: text });
  await expect(bubble.getByRole('button', { name: 'Изменить' })).toBeVisible();
  return { bubble, inputId };
}

// Поле правки — то же поле приложения, что и в формах, а не серый прямоугольник браузера.
async function expectThemedField(field: Locator) {
  const style = await field.evaluate((element) => {
    const probe = document.createElement('div');
    probe.style.background = 'var(--raised)';
    document.body.append(probe);
    const raised = getComputedStyle(probe).backgroundColor;
    probe.remove();
    const own = getComputedStyle(element);
    return { background: own.backgroundColor, raised, border: own.borderTopWidth, resize: own.resize, radius: parseFloat(own.borderTopLeftRadius) };
  });
  expect(style.background).toBe(style.raised);
  expect(style.border).toBe('0px');
  expect(style.resize).toBe('none');
  expect(style.radius).toBeGreaterThan(0);
}

test('edit_queued_bubble_in_place_and_save', async ({ page }) => {
  const { hub, api, id } = await boot(page, []);
  const { ownKey } = await sendOwn(page, 'Черновик в очереди');
  expect(ownKey).not.toBe('');
  const { bubble } = await confirmQueued(page, hub, api, id, ownKey, 'Черновик в очереди');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') });
  await expect(editor.getByLabel('Текст сообщения')).toBeVisible();
  await expectThemedField(editor.getByLabel('Текст сообщения'));
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
  await expectThemedField(item.getByLabel('Текст сообщения'));
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

test('edit_strip_conflict_uses_revision_from_start', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Правка',
    harness: { id: 'codex', title: 'codex', provider: 'x' }
  } as never);
  const runningInput = acceptedInput(api, '1', 'В ход');
  const queued = queuedInput(api, '2', 'В очереди');
  api.histories.set(id, [runningInput, queued] as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  turnRunning(hub, id, runningInput.id);
  const item = page.locator('.strip .item', { hasText: 'В очереди' });
  await expect(item.getByRole('button', { name: 'Изменить' })).toBeVisible();
  await item.getByRole('button', { name: 'Изменить' }).click();
  hub.append(`conversation:${id}`, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: {
      input_id: queued.id,
      state: 'queued',
      attempt: 0,
      cause: 'edited',
      revision: 2,
      content: [{ type: 'text', text: 'Чужое новое' }]
    }
  });
  hub.flush();
  const bodies: { text?: string; expected_revision?: number }[] = [];
  await page.route('**/api/v1/conversations/*/inputs/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    bodies.push(route.request().postDataJSON());
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
  await item.locator('textarea').fill('Моё');
  await item.getByRole('button', { name: 'Сохранить' }).click();
  await expect.poll(() => bodies.length).toBe(1);
  expect(bodies[0]).toMatchObject({ text: 'Моё', expected_revision: 1 });
  await expect(item.getByText('Сообщение уже изменено в другом месте')).toBeVisible();
  await expect(item.locator('textarea')).toHaveValue('Чужое новое');
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

test('edit_conflict_keeps_the_editor_open_with_the_other_revision', async ({ page }) => {
  const { hub, api, id } = await boot(page, []);
  const { ownKey } = await sendOwn(page, 'Моё');
  const { bubble, inputId } = await confirmQueued(page, hub, api, id, ownKey, 'Моё');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') });
  // Другая сессия успела сохранить свою правку до нашего «Сохранить».
  hub.append(`conversation:${id}`, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: {
      input_id: inputId,
      state: 'queued',
      attempt: 0,
      cause: 'edited',
      revision: 2,
      content: [{ type: 'text', text: 'Чужое новое' }]
    }
  });
  hub.flush();
  const bodies: { text?: string; expected_revision?: number }[] = [];
  let calls = 0;
  await page.route('**/api/v1/conversations/*/inputs/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    bodies.push(route.request().postDataJSON());
    calls += 1;
    if (calls === 1) {
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
      return;
    }
    await route.fulfill({ json: { data: { input_id: inputId, revision: 3 } } });
  });
  await editor.locator('textarea').fill('Моё');
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect.poll(() => bodies.length).toBe(1);
  // Отправляется ревизия, снятая при открытии редактора, а не текущая.
  expect(bodies[0]).toMatchObject({ text: 'Моё', expected_revision: 1 });
  await expect(editor.getByText('Сообщение уже изменено в другом месте')).toBeVisible();
  await expect(editor.locator('textarea')).toHaveValue('Чужое новое');

  // Вторая правка идёт уже с ревизией, показанной после конфликта.
  await editor.locator('textarea').fill('Моё после конфликта');
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect.poll(() => bodies.length).toBe(2);
  expect(bodies[1]).toMatchObject({ text: 'Моё после конфликта', expected_revision: 2 });
  hub.append(`conversation:${id}`, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: {
      input_id: inputId,
      state: 'queued',
      attempt: 0,
      cause: 'edited',
      revision: 3,
      content: [{ type: 'text', text: 'Моё после конфликта' }]
    }
  });
  hub.flush();
  await expect(page.locator('.bubble').filter({ has: page.locator('textarea') })).toHaveCount(0);
  await expect(page.locator('.bubble', { hasText: 'Моё после конфликта' })).toBeVisible();
});

test('background_update_keeps_an_open_bubble_edit', async ({ page, context, baseURL }) => {
  const { hub, api, id } = await boot(page, []);
  const { ownKey } = await sendOwn(page, 'Правка до обновления');
  const { bubble } = await confirmQueued(page, hub, api, id, ownKey, 'Правка до обновления');
  await bubble.getByRole('button', { name: 'Изменить' }).click();
  const editor = page.locator('.bubble').filter({ has: page.locator('textarea') }).locator('textarea');
  await editor.fill('несохранённый текст');
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await expect(page.locator('[data-update]')).toBeAttached({ timeout: 10_000 });
  await page.evaluate(() => {
    (window as unknown as { __marker: boolean }).__marker = true;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await page.waitForTimeout(1000);
  expect(await page.evaluate(() => (window as unknown as { __marker?: boolean }).__marker)).toBe(true);
  await expect(editor).toHaveValue('несохранённый текст');
});
