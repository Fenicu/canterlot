import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

function queuedInput(api: FakeHubApi, seq: string, text: string) {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `k-${seq}`,
    state: 'queued',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: seq,
    feed_seq: seq,
    created_at: AT
  };
}

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

async function boot(page: import('@playwright/test').Page, harness = 'codex', queued = true) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Отправка',
    harness: { id: harness, title: harness, provider: 'x' }
  } as never);
  const items = [acceptedInput(api, '1', 'В ход')];
  if (queued) items.push(queuedInput(api, '2', 'В очередь'));
  api.histories.set(id, items as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function working(hub: FakeHub, api: FakeHubApi, id: string, inputId: string) {
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
    payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } }
  });
  hub.flush();
}

test('send_now_buttons_appear_while_a_turn_runs_and_post_to_the_hub', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const inputId = (api.histories.get(id) as { id: string }[])[1]!.id;
  working(hub, api, id, (api.histories.get(id) as { id: string }[])[0]!.id);
  // Полоса очереди показывает ожидающий ввод хаба.
  const buttons = page.getByRole('button', { name: 'Отправить сейчас' });
  await expect(buttons).toHaveCount(1);
  // «Отправить сейчас» не повторяет стрелку отправки из поля ввода.
  const glyph = (locator: import('@playwright/test').Locator) => locator.locator('svg path').first().getAttribute('d');
  expect(await glyph(buttons.first())).not.toBe(await glyph(page.getByRole('button', { name: 'Отправить', exact: true })));
  const hits: string[] = [];
  await page.route('**/send-now', async (route) => {
    hits.push(route.request().url());
    await route.fulfill({ json: { data: { requested: { steer_command_id: api.id() } } } });
  });
  await buttons.first().click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0]).toContain(`/api/v1/conversations/${id}/inputs/${inputId}/send-now`);
  await expect(page.getByText('Вливается в идущий ход').first()).toBeVisible();
});

test('rejected_steer_keeps_the_message_queued_with_a_caption', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const inputId = (api.histories.get(id) as { id: string }[])[1]!.id;
  working(hub, api, id, (api.histories.get(id) as { id: string }[])[0]!.id);
  const steer = api.id();
  await page.route('**/send-now', async (route) => {
    await route.fulfill({ json: { data: { requested: { steer_command_id: steer } } } });
  });
  await page.getByRole('button', { name: 'Отправить сейчас' }).first().click();
  await expect(page.getByText('Вливается в идущий ход').first()).toBeVisible();
  // Агент отказался принять сообщение в идущий ход: оно остаётся в очереди.
  hub.append(`conversation:${id}`, {
    kind: 'input_state_changed',
    conversation_id: id,
    payload: {
      input_id: inputId,
      state: 'queued',
      attempt: 0,
      cause: 'steer_rejected',
      command_id: steer,
      reason: 'Internal error'
    }
  });
  hub.flush();
  await expect(page.getByText('Не удалось влить в ход: сообщение уйдёт следующим').first()).toBeVisible();
  await expect(page.getByText('Вливается в идущий ход')).toHaveCount(0);
  await expect(page.locator('.strip').getByText('В очередь')).toBeVisible();
});

test('send_now_button_appears_on_the_queued_owner_bubble', async ({ page }) => {
  const { hub, api, id } = await boot(page, 'codex', false);
  // Своё сообщение, отправленное без хода, висит пузырём, но хода нет —
  // кнопки «Отправить сейчас» нет.
  let ownKey = '';
  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    ownKey = route.request().headers()['idempotency-key'] ?? '';
    return route.fallback();
  });
  await page.getByRole('textbox', { name: 'Сообщение' }).fill('Догоняющее сообщение');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  const bubble = page.locator('.bubble', { hasText: 'Догоняющее сообщение' });
  const stripButton = page.locator('.strip').getByRole('button', { name: 'Отправить сейчас' });
  await expect(bubble).toBeVisible();
  await expect(bubble.getByRole('button', { name: 'Отправить сейчас' })).toHaveCount(0);
  // Ход начался: ввод ушёл в полосу очереди, кнопка появилась там.
  // Приём хаба доводим событием с тем же ключом идемпотентности.
  working(hub, api, id, (api.histories.get(id) as { id: string }[])[0]!.id);
  await expect(bubble).toHaveCount(0);
  hub.append(`conversation:${id}`, {
    kind: 'input_queued',
    conversation_id: id,
    payload: {
      input_id: api.id(),
      input_seq: '3',
      idempotency_key: ownKey,
      state: 'queued',
      content: [{ type: 'text', text: 'Догоняющее сообщение' }]
    }
  });
  hub.flush();
  await expect(stripButton).toBeVisible();
  const hits: string[] = [];
  await page.route('**/send-now', async (route) => {
    hits.push(route.request().url());
    await route.fulfill({ json: { data: { requested: { steer_command_id: api.id() } } } });
  });
  await stripButton.click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0]).toContain('/inputs/');
  expect(hits[0]).toContain('/send-now');
});

test('send_now_hidden_without_a_running_turn', async ({ page }) => {
  await boot(page);
  await expect(page.getByRole('button', { name: 'Отправить сейчас' })).toHaveCount(0);
});

test('send_now_refusal_shows_a_caption', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  working(hub, api, id, (api.histories.get(id) as { id: string }[])[0]!.id);
  await page.route('**/send-now', async (route) => {
    await route.fulfill({
      status: 400,
      json: {
        error: {
          code: 'invalid_request',
          message: 'Некорректный запрос',
          scope: 'browser',
          retry: 'never',
          correlation_id: '00000000-0000-4000-8000-0000000000f1',
          details: { reason: 'request_open' }
        }
      }
    });
  });
  await expect(page.getByRole('button', { name: 'Отправить сейчас' })).toHaveCount(1);
  await page.getByRole('button', { name: 'Отправить сейчас' }).first().click();
  await expect(page.getByText('Дождитесь ответа на запрос').first()).toBeVisible();
});

test('opencode_send_now_warns_that_the_step_is_interrupted', async ({ page }) => {
  const { hub, api, id } = await boot(page, 'opencode');
  working(hub, api, id, (api.histories.get(id) as { id: string }[])[0]!.id);
  await expect(page.getByRole('button', { name: 'Отправить сейчас' })).toHaveCount(1);
  await expect(page.getByText('Прервёт текущий шаг').first()).toBeVisible();
});

test('interrupted_turn_line_is_shown_for_a_live_execution', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'interrupt_state_changed',
    conversation_id: id,
    payload: { command_id: api.id(), status: 'completed' }
  });
  hub.append(scope, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: {
      pause_reasons: [{ reason: 'user_stop' }],
      hold_reasons: [],
      run_slot: 'free'
    }
  });
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'idle', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 1, pause_reasons: [{ reason: 'user_stop' }], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.flush();
  await expect(page.getByText('Прервано')).toBeVisible();
  await expect(page.getByText('Очередь остановлена')).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeEnabled();
});
