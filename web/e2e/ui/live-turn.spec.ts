import type { Page } from '@playwright/test';
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

async function boot(page: Page, opts: { harness?: string; queued?: string } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, {
    title: 'Ход идёт',
    harness: { id: opts.harness ?? 'codex', title: opts.harness ?? 'codex', provider: 'x' }
  } as never);
  const items: unknown[] = [
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'k-turn',
      state: 'accepted',
      content: [{ type: 'text', text: 'Покажи план' }],
      attachments: [],
      created_seq: '1',
      feed_seq: '1',
      created_at: AT
    }
  ];
  if (opts.queued) items.push(queuedInput(api, '2', opts.queued));
  api.histories.set(id, items as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

/** Ход идёт: очередь занята, живая строка видна, ответа ещё нет. */
function startTurn(hub: FakeHub, api: FakeHubApi, id: string): string {
  const scope = `conversation:${id}`;
  const inputId = (api.histories.get(id) as { id: string }[])[0]!.id;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 1, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } } });
  hub.flush();
  return inputId;
}

test('a_live_turn_promises_a_queue_and_names_the_send_button', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const composer = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(composer).toHaveAttribute('placeholder', 'Сообщение агенту…');
  await expect(page.getByRole('button', { name: 'Отправить', exact: true })).toBeVisible();

  startTurn(hub, api, id);

  await expect(composer).toHaveAttribute('placeholder', 'Ход идёт: сообщение встанет в очередь');
  const send = page.getByRole('button', { name: 'В очередь', exact: true });
  await expect(send).toBeVisible();
  await expect(send).toHaveAttribute('title', 'В очередь');
  // «Влить в ход» стоит рядом с отправкой в той же строке.
  const steer = await page.getByRole('button', { name: 'Влить в ход' }).boundingBox();
  const box = (await send.boundingBox())!;
  expect(steer!.y).toBeCloseTo(box.y, 0);
  expect(steer!.x).toBeLessThan(box.x);
});

test('steering_from_the_composer_reaches_the_send_now_path', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  startTurn(hub, api, id);
  const hits: string[] = [];
  await page.route('**/send-now', async (route) => {
    hits.push(route.request().url());
    await route.fulfill({ json: { data: { requested: { steer_command_id: api.id() } } } });
  });
  const composer = page.getByRole('textbox', { name: 'Сообщение' });
  await composer.fill('Сначала покажи план');
  await page.getByRole('button', { name: 'Влить в ход' }).click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0]).toContain(`/api/v1/conversations/${id}/inputs/`);
  expect(hits[0]).toContain('/send-now');
  await expect(composer).toHaveValue('');
});

test('the_queue_row_names_the_steer_action_and_keeps_two_lines_of_text', async ({ page }, testInfo) => {
  const phone = testInfo.project.name === 'phone';
  const long = 'И не забудь про индексы: они должны быть готовы до миграции, иначе ночной прогон встанет';
  const { hub, api, id } = await boot(page, { queued: long });
  startTurn(hub, api, id);

  const strip = page.locator('.strip');
  const steer = strip.getByRole('button', { name: /^Влить: / });
  await expect(steer).toBeVisible();
  const text = strip.locator('.text').first();
  expect(await text.evaluate((node) => getComputedStyle(node).webkitLineClamp)).toBe('2');
  const box = (await text.boundingBox())!;
  const line = await text.evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));
  expect(box.height).toBeGreaterThan(line * 1.6);

  if (phone) {
    await expect(strip.getByRole('button', { name: /^Изменить:/ })).toBeHidden();
    const more = strip.getByRole('button', { name: /^Действия с сообщением:/ }).first();
    await expect(more).toBeVisible();
    await more.click();
    const menu = strip.getByRole('menu');
    await expect(menu.getByRole('menuitem', { name: 'Изменить' })).toBeVisible();
    await expect(menu.getByRole('menuitem', { name: 'Отменить' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
  } else {
    await expect(strip.getByRole('button', { name: /^Изменить:/ })).toBeVisible();
    await expect(strip.getByRole('button', { name: /^Действия с сообщением:/ }).first()).toBeHidden();
  }
});

test('an_interrupting_harness_says_so_on_the_steer_button_itself', async ({ page }) => {
  const { hub, api, id } = await boot(page, { harness: 'opencode', queued: 'Сначала тесты' });
  startTurn(hub, api, id);
  const strip = page.locator('.strip');
  await expect(strip.getByRole('button', { name: /^Влить \(прервёт шаг\): / })).toBeVisible();
  await expect(strip.getByText('Прервёт текущий шаг')).toHaveCount(0);
});
