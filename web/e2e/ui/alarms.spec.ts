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

test('alarm_entry_is_collapsed_and_expands_to_the_full_batch_text', async ({ page }) => {
  const { api, id } = await boot(page);
  const summary = [
    '[canterlot] Alarm "wave" fired: sessions 00000000-0000-4000-8000-0000000000e5, 00000000-0000-4000-8000-0000000000e6 settled (all, batched).',
    '',
    '## Итог сессии «Сборка»',
    '',
    'Собрал отчёт и оставил заметки по миграции.',
    '',
    '## Итог сессии «Тесты»',
    '',
    'Прогнал набор и починил два падения.',
    '',
    'Note: Гейт для волны C.'
  ].join('\n');
  seedInput(api, id, {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'alarm-batch',
    state: 'accepted',
    content: [{ type: 'text', text: summary }],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT,
    origin: {
      kind: 'alarm',
      chain_depth: 0,
      event: { kind: 'alarm', alarm_key: 'wave', fired_reason: 'condition' }
    }
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  const entry = page.locator('[data-role="alarm"]');
  await expect(entry).toBeVisible();
  await expect(entry).toContainText('Будильник «wave»: условие выполнено');
  const toggle = entry.getByRole('button');
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(entry.locator('[data-role="alarm-body"]')).toHaveCount(0);
  await expect(entry).not.toContainText('Гейт для волны C');

  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  const body = entry.locator('[data-role="alarm-body"]');
  await expect(body).toContainText('Итог сессии «Сборка»');
  await expect(body).toContainText('Собрал отчёт и оставил заметки по миграции.');
  await expect(body).toContainText('Note: Гейт для волны C');

  // Клавиатура закрывает и снова открывает тот же вход.
  await toggle.press('Space');
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(entry.locator('[data-role="alarm-body"]')).toHaveCount(0);
  await toggle.press('Enter');
  await expect(body).toBeVisible();

  for (const palette of ['aquamarine', 'standard', 'vscode']) {
    for (const theme of ['dark', 'light']) {
      await page.evaluate(([nextPalette, nextTheme]) => {
        document.documentElement.dataset.palette = nextPalette!;
        document.documentElement.dataset.theme = nextTheme!;
      }, [palette, theme]);
      await expect(body).toBeVisible();
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        `${palette} ${theme}`
      ).toBe(true);
    }
  }
});

test('coalesced_alarm_entry_counts_the_fired_alarms_in_the_header', async ({ page }) => {
  const { api, id } = await boot(page);
  const summary = [
    '[canterlot] 3 alarms fired.',
    '',
    '- [canterlot] Alarm "gate" fired at 14:30 UTC.',
    '- [canterlot] Alarm "disk" fired: session 00000000-0000-4000-8000-0000000000e5 is idle.',
    '- [canterlot] Alarm timed out after 10m: 0/1 sessions settled (batched). Not settled: 00000000-0000-4000-8000-0000000000e6.',
    '',
    'This is canterlot automation, not the owner.'
  ].join('\n');
  seedInput(api, id, {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'alarm-coalesced',
    state: 'accepted',
    content: [{ type: 'text', text: summary }],
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

  const entry = page.locator('[data-role="alarm"]');
  await expect(entry.locator('.line')).toHaveText('Сработали 3 будильника');
  await expect(entry).not.toContainText('«gate»');
  await expect(entry.locator('[data-role="alarm-body"]')).toHaveCount(0);

  await entry.getByRole('button').click();
  const body = entry.locator('[data-role="alarm-body"]');
  await expect(body).toContainText('Alarm "disk"');
  await expect(body).toContainText('Not settled');
});
