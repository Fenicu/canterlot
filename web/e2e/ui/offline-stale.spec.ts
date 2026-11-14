import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

const banner = (page: Page) => page.locator('[data-banner]');
const staleRoot = (page: Page) => page.locator('.shell[data-stale]');

async function bootChat(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  api.histories.set(id, [
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'k-turn',
      state: 'accepted',
      content: [{ type: 'text', text: 'Покажи миграцию' }],
      attachments: [],
      created_seq: '1',
      feed_seq: '1',
      created_at: AT
    }
  ] as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function startTurn(hub: FakeHub, api: FakeHubApi, id: string) {
  const scope = `conversation:${id}`;
  const inputId = (api.histories.get(id) as { id: string }[])[0]!.id;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } } });
  hub.flush();
}

/** Цвет элемента сравнивается с токеном темы, а не с жёстким значением палитры. */
async function colorAndToken(page: Page, target: Locator, token: string): Promise<{ actual: string; expected: string }> {
  return target.evaluate((node, name) => {
    const probe = document.createElement('span');
    probe.style.color = `var(${name})`;
    document.body.appendChild(probe);
    const expected = getComputedStyle(probe).color;
    probe.remove();
    return { actual: getComputedStyle(node).color, expected };
  }, token);
}

test('a_live_timer_freezes_after_five_seconds_offline_and_moves_again_on_reconnect', async ({ page }) => {
  test.setTimeout(60_000);
  const { hub, api, id } = await bootChat(page);
  startTurn(hub, api, id);
  const line = page.locator('[data-role="live-turn"]');
  await expect(line).toContainText(/Генерирует · \d+ с/);

  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  await expect(staleRoot(page)).toHaveCount(1, { timeout: 15_000 });
  await expect(banner(page)).toContainText(/· данные на \d{2}:\d{2}/);

  // Живой тон гаснет до токена muted, а не до акцента.
  const write = line.locator('.write');
  const tone = await colorAndToken(page, write, '--muted');
  expect(tone.actual).toBe(tone.expected);

  const frozen = await line.textContent();
  await page.waitForTimeout(2200);
  expect(await line.textContent()).toBe(frozen);

  hub.acceptConnections = true;
  // Возврат сети просит клиента проверить связь сразу, не дожидаясь длинной паузы.
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect(banner(page)).toContainText('Связь восстановлена', { timeout: 25_000 });
  await expect(staleRoot(page)).toHaveCount(0);
  const moving = await line.textContent();
  await expect.poll(async () => line.textContent(), { timeout: 5_000 }).not.toBe(moving);
});

test('the_banner_owns_a_row_and_does_not_cover_the_first_section_header', async ({ page }, testInfo) => {
  const phone = testInfo.project.name === 'phone';
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });

  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  const card = (await banner(page).boundingBox())!;
  const header = (await page.locator('[data-list-scroll] .section-head').first().boundingBox())!;
  if (phone) {
    // Телефон: у баннера своя полоса над списком, поэтому первая секция не прячется.
    expect(card.y + card.height).toBeLessThanOrEqual(header.y + 1);
    expect(await banner(page).evaluate((node) => getComputedStyle(node.parentElement!).position)).toBe('static');
  } else {
    // Стол: баннер живёт в панели чата и списка не касается.
    const list = (await page.locator('[data-pane="list"]').boundingBox())!;
    expect(card.x).toBeGreaterThanOrEqual(list.x + list.width - 1);
  }
  hub.acceptConnections = true;
});

test('a_message_sent_without_a_link_waits_for_it_instead_of_pretending_to_save', async ({ page }) => {
  const { hub } = await bootChat(page);
  // Сокет оборван, POST не доходит: ввод остаётся на устройстве.
  hub.acceptConnections = false;
  hub.chaos.cut();
  await expect(banner(page)).toContainText('Нет связи. Переподключаюсь…', { timeout: 8_000 });
  await page.route('**/api/v1/conversations/*/inputs', (route) => route.abort('internetdisconnected'));

  await page.getByRole('textbox', { name: 'Сообщение' }).fill('Отправь, когда будет связь');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();

  const strip = page.locator('.bubble', { hasText: 'Отправь, когда будет связь' });
  await expect(strip).toContainText('Ждёт связи');
  await expect(strip).toContainText('отправлю, когда связь вернётся');
  await expect(strip).not.toContainText('Сохраняется');
  hub.acceptConnections = true;
});
