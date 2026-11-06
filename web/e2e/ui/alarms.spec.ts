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

const alarmsList = (page: Page) => page.locator('[data-alarms-list]');
const alarmsPanel = (page: Page) => page.locator('[data-alarms-panel]');
const alarmMenuItem = (page: Page) => page.getByRole('menuitem', { name: /^Будильники · \d+$/ });

async function openMenu(page: Page) {
  await page.getByRole('button', { name: 'Меню сессии' }).click();
}

async function openAlarms(page: Page) {
  await openMenu(page);
  await alarmMenuItem(page).click();
}

async function closeAlarms(page: Page) {
  if (test.info().project.name === 'phone') {
    await page.getByRole('dialog', { name: 'Будильники' }).getByRole('button', { name: 'Закрыть' }).click();
  } else {
    await alarmsPanel(page).getByRole('button', { name: 'Закрыть боковую панель' }).click();
  }
}

function alarmAt(index: number, patch: Record<string, unknown> = {}) {
  return alarm({
    alarm_id: `00000000-0000-4000-8000-0000000000b${index}`,
    key: `alarm-${index}`,
    fires_at: `2026-11-04T1${index}:30:00.000Z`,
    ...patch
  });
}

function seedAlarms(hub: FakeHub, id: string, alarms: ReturnType<typeof alarmAt>[]) {
  for (const item of alarms) {
    hub.append(`conversation:${id}`, {
      kind: 'alarm_changed',
      conversation_id: id,
      payload: { ...item, conversation_id: id }
    });
  }
}

test('alarm_menu_item_opens_the_list_and_cancel_works', async ({ page }) => {
  const { hub, id } = await boot(page);
  hub.append(`conversation:${id}`, {
    kind: 'alarm_changed',
    conversation_id: id,
    payload: alarm({ conversation_id: id })
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  await expect(alarmsList(page)).toHaveCount(0);
  await openMenu(page);
  await expect(alarmMenuItem(page)).toHaveText('Будильники · 1');
  await alarmMenuItem(page).click();
  await expect(alarmsList(page)).toBeVisible();
  await alarmsList(page).locator('.chip').click();
  await expect(alarmsList(page)).toContainText('Гейт для волны C');

  await alarmsList(page).getByRole('button', { name: /Отменить/ }).click();
  await page.getByRole('button', { name: 'Отменить', exact: true }).click();
  await expect(alarmsList(page).locator('.pill')).toHaveCount(0);

  await closeAlarms(page);
  await openMenu(page);
  await expect(alarmMenuItem(page)).toHaveCount(0);
});

test('file_alarm_names_the_path_and_deadline_in_russian', async ({ page }) => {
  const { hub, id } = await boot(page);
  hub.append(`conversation:${id}`, {
    kind: 'alarm_changed',
    conversation_id: id,
    payload: alarm({
      conversation_id: id,
      alarm_id: '00000000-0000-4000-8000-0000000000a4',
      key: 'gate',
      kind: 'condition',
      condition:
        'file /tmp/gate.rc appears (at least 2 bytes, written after arming); deadline 2026-11-04 18:00 UTC',
      fires_at: null,
      deadline_at: '2026-11-04T18:00:00.000Z',
      path: '/tmp/gate.rc',
      min_bytes: 2,
      fresh: true,
      read_head: true
    })
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  await openAlarms(page);
  const pill = alarmsList(page).locator('.pill').first();
  await expect(pill).toContainText('Будильник · «gate» · когда появится /tmp/gate.rc · до ');
  await pill.locator('.chip').click();
  await expect(alarmsList(page)).toContainText(
    'Условие: появится /tmp/gate.rc, не меньше 2 байт, только после взвода'
  );
});

test('fired_file_alarm_is_a_quiet_feed_line_with_the_path_and_size', async ({ page }) => {
  const { api, id } = await boot(page);
  seedInput(api, id, {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'alarm-file',
    state: 'accepted',
    content: [
      {
        type: 'text',
        text: '[canterlot] Alarm "gate" fired: file /tmp/gate.rc appeared (2 bytes, written 14:31 UTC). Head: "0".'
      }
    ],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT,
    origin: {
      kind: 'alarm',
      chain_depth: 0,
      event: {
        kind: 'alarm',
        alarm_key: 'gate',
        fired_reason: 'condition',
        alarm_path: '/tmp/gate.rc',
        alarm_size: 2
      }
    }
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();
  await expect(page.locator('.line[data-event-kind="alarm"]')).toContainText(
    'Будильник «gate»: появился /tmp/gate.rc (2 байта)'
  );
});

test('machine_alarm_describes_its_thresholds_in_russian_in_the_list', async ({ page }) => {
  const { hub, id } = await boot(page);
  hub.append(`conversation:${id}`, {
    kind: 'alarm_changed',
    conversation_id: id,
    payload: alarm({
      conversation_id: id,
      alarm_id: '00000000-0000-4000-8000-0000000000a3',
      key: 'freed',
      kind: 'condition',
      condition: 'machine: free disk above 50 GB, load below 0.5 per core; deadline 2026-11-05 12:00 UTC',
      fires_at: null,
      deadline_at: '2026-11-05T12:00:00.000Z',
      free_disk_gb_above: 50,
      load_per_core_below: 0.5,
      mem_available_gb_above: 8
    })
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  await openAlarms(page);
  await expect(alarmsList(page)).toContainText(
    'Машина: свободно на диске больше 50 ГБ, нагрузка на ядро ниже 0,5, память больше 8 ГБ'
  );
});

test('alarm_event_is_a_quiet_feed_line_and_a_live_change_adds_a_menu_item', async ({ page }) => {
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
  await openMenu(page);
  await expect(alarmMenuItem(page)).toHaveCount(0);
  await page.keyboard.press('Escape');

  hub.append(`conversation:${id}`, {
    kind: 'alarm_changed',
    conversation_id: id,
    payload: alarm({
      conversation_id: id,
      alarm_id: '00000000-0000-4000-8000-0000000000a2',
      key: 'disk'
    })
  });
  await openMenu(page);
  await expect(alarmMenuItem(page)).toHaveText('Будильники · 1');
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

test('several_alarms_live_in_the_menu_and_count_updates_after_cancel', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  seedAlarms(hub, id, [alarmAt(1), alarmAt(2), alarmAt(3), alarmAt(4)]);
  api.pushTodo(id, [
    { text: 'Проверить полосы', status: 'in_progress' },
    { text: 'Снять скриншоты', status: 'pending' }
  ]);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  // Над композером полосы больше нет: будильники живут только в меню.
  await expect(page.locator('.screen > [data-alarms-list]')).toHaveCount(0);
  await openAlarms(page);
  await expect(alarmsList(page).locator('.pill')).toHaveCount(4);
  await expect(alarmsList(page).getByRole('button', { name: /Отменить:/ })).toHaveCount(4);

  await alarmsList(page).getByRole('button', { name: /Отменить:/ }).first().click();
  await page.getByRole('button', { name: 'Отменить', exact: true }).click();
  await expect(alarmsList(page).locator('.pill')).toHaveCount(3);

  await closeAlarms(page);
  await openMenu(page);
  await expect(alarmMenuItem(page)).toHaveText('Будильники · 3');
});

test('alarm_menu_item_is_absent_without_alarms', async ({ page }) => {
  const { id } = await boot(page);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  await expect(alarmsList(page)).toHaveCount(0);
  await openMenu(page);
  await expect(alarmMenuItem(page)).toHaveCount(0);
  await expect(page.getByRole('menuitem', { name: /Будильники/ })).toHaveCount(0);
});

test('todo_keeps_the_vertical_rhythm_with_the_next_strip', async ({ page }) => {
  const { api, id } = await boot(page);
  api.pushTodo(id, [{ text: 'Проверить отступы', status: 'in_progress' }]);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();
  await expect(page.locator('.todo .panel')).toBeVisible();

  const gap = await page.evaluate(() => {
    const todo = document.querySelector('.todo .panel')!.getBoundingClientRect();
    const next = document.querySelector('.screen > .strip')!.getBoundingClientRect();
    const s2 = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--s2'));
    return { value: next.top - todo.bottom, token: s2 };
  });
  expect(gap.value).toBeGreaterThanOrEqual(gap.token);
});
