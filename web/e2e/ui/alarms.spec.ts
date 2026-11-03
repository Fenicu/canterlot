import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation, seedInput } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-11-04T14:30:00.000Z';

function alarm(patch: Record<string, unknown> = {}) {
  return {
    alarm_id: '00000000-0000-4000-8000-0000000000a1',
    conversation_id: '',
    key: 'gate',
    kind: 'at',
    condition: 'at 2026-11-04 14:30 UTC',
    session_ids: [],
    states: [],
    fires_at: AT,
    deadline_at: null,
    only_on_timeout: false,
    batch: false,
    state: 'armed',
    fired_reason: null,
    failure_reason: null,
    fired_at: null,
    note: 'Гейт для волны C',
    note_bytes: 16,
    created_at: '2026-11-04T12:30:00.000Z',
    ...patch
  };
}

async function boot(page: Page) {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 384 : 1280, height: 844 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Оркестратор' });
  await installChatRoutes(page, hub, api, OWNER);
  return { hub, api, id };
}

const strip = (page: Page) => page.getByRole('region', { name: 'Будильники' });

test('armed_alarm_plaque_shows_the_note_and_can_be_cancelled', async ({ page }) => {
  const { hub, id } = await boot(page);
  hub.append(`conversation:${id}`, {
    kind: 'alarm_changed',
    conversation_id: id,
    payload: alarm({ conversation_id: id })
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  await expect(strip(page)).toContainText('Будильник');
  await strip(page).locator('.chip').click();
  await expect(strip(page)).toContainText('Гейт для волны C');

  await strip(page).getByRole('button', { name: /Отменить/ }).click();
  await page.getByRole('button', { name: 'Отменить', exact: true }).click();
  await expect(strip(page)).toHaveCount(0);
});

test('alarm_event_is_a_quiet_feed_line_and_a_live_change_adds_a_plaque', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  seedInput(api, id, {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'alarm-1',
    state: 'accepted',
    content: [{ type: 'text', text: '[canterlot] Alarm "gate" fired at 14:30 UTC.' }],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT,
    origin: {
      kind: 'alarm',
      chain_depth: 0,
      event: { kind: 'alarm', alarm_key: 'gate', fired_reason: 'time' }
    }
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();
  await expect(page.locator('.line[data-event-kind="alarm"]')).toContainText('Будильник «gate»');

  hub.append(`conversation:${id}`, {
    kind: 'alarm_changed',
    conversation_id: id,
    payload: alarm({
      conversation_id: id,
      alarm_id: '00000000-0000-4000-8000-0000000000a2',
      key: 'disk'
    })
  });
  await expect(strip(page)).toContainText('Будильник');
});
