import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation, seedInput } from '../../testing/fake-hub/seed';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

function rule(patch: Record<string, unknown> = {}) {
  return {
    session_id: '',
    enabled: true,
    state: 'watching',
    idle_minutes: 3,
    background_minutes: 10,
    max_consecutive: 3,
    max_total: 20,
    done_markers: ['STATUS: DONE', 'STATUS: BLOCKED'],
    context_warn_percent: 80,
    set_by: 'agent',
    set_by_session: null,
    notify_session_id: null,
    consecutive: 0,
    total: 2,
    held: false,
    ...patch,
  };
}

async function bootPresets(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  seedPresets(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/presets');
  await expect(page.locator('[data-presets-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

async function bootChat(page: Page) {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 384 : 1280, height: 844 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Исполнитель' });
  api.nudgeRules.set(id, rule({ session_id: id }));
  api.nudgeEvents.set(id, [
    {
      id: '00000000-0000-4000-8000-0000000000b2',
      at: '2026-11-05T12:02:00.000Z',
      attempt: 2,
      outcome: 'nudged',
      signals: { no_marker: true, continue_phrase: true },
    },
  ]);
  seedInput(api, id, {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'nudge-line-1',
    state: 'accepted',
    content: [
      {
        type: 'text',
        text: '[canterlot] Automatic reminder from canterlot, not the owner: continue.',
      },
    ],
    attachments: [],
    created_seq: '1',
    created_at: '2026-11-05T12:02:00.000Z',
    feed_seq: '1',
    origin: {
      kind: 'nudge',
      rule_id: id,
      attempt: 2,
      max: 3,
      chain_depth: 0,
      signals: { no_marker: true, continue_phrase: true },
      event: { kind: 'nudge', session_id: id },
    },
  });
  seedInput(api, id, {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'owner-message-1',
    state: 'accepted',
    content: [{ type: 'text', text: 'Обычное сообщение владельца' }],
    attachments: [],
    created_seq: '2',
    created_at: '2026-11-05T12:03:00.000Z',
    feed_seq: '2',
    origin: null,
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

test('nudge_text_tab_lists_the_builtin_and_creates_edits_and_deletes_a_preset', async ({ page }) => {
  await bootPresets(page);
  await page.locator('[data-purpose-tab="nudge"]').click();
  const builtin = page.locator('[data-preset="nudge_default"]');
  await expect(builtin).toContainText('Продолжай (английский)');
  await expect(builtin).toContainText('встроенный');
  await expect(builtin.getByRole('button', { name: 'Изменить' })).toHaveCount(0);

  await page.getByRole('button', { name: 'Новый текст толчка' }).click();
  await page.getByLabel('Название').fill('Свой толчок');
  await page.getByRole('textbox', { name: 'Текст' }).fill('[canterlot] Продолжай. {markers}. {signals}.');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  const custom = page.locator('[data-preset]').filter({ hasText: 'Свой толчок' });
  await expect(custom).toBeVisible();
  await custom.getByRole('button', { name: 'Изменить' }).click();
  await page.getByRole('textbox', { name: 'Текст' }).fill('[canterlot] Продолжай ещё. {markers}.');
  await page.getByRole('button', { name: 'Сохранить' }).click();
  await expect(custom.getByText('версия 2')).toBeVisible();

  // Вкладка инструкций не показывает текст толчка.
  await page.locator('[data-purpose-tab="instructions"]').click();
  await expect(page.locator('[data-preset]').filter({ hasText: 'Свой толчок' })).toHaveCount(0);

  await page.locator('[data-purpose-tab="nudge"]').click();
  await custom.getByRole('button', { name: 'Удалить' }).click();
  await expect(page.getByText('Правила, где он выбран')).toBeVisible();
  await page.getByRole('button', { name: 'Удалить текст толчка' }).click();
  await expect(page.locator('[data-preset]').filter({ hasText: 'Свой толчок' })).toHaveCount(0);
});

test('delegate_header_and_sheet_show_the_rule_and_pause_resume_clear', async ({ page }) => {
  await bootChat(page);
  const line = page.locator('[data-role="nudge-state-line"]');
  await expect(line).toHaveText('Автопинок: следит');

  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Автопинок' }).click();
  const sheet = page.locator('[data-role="nudge-sheet"]');
  await expect(sheet).toBeVisible();
  await expect(sheet.locator('[data-role="nudge-state"]')).toHaveText('Автопинок: следит');
  await expect(sheet.locator('[data-role="nudge-counter"]')).toContainText('толчков 2 из 20');
  await expect(sheet.locator('[data-role="nudge-last-signals"]')).toContainText(
    'нет маркера готовности, обещал продолжить',
  );

  await sheet.getByRole('button', { name: 'Поставить на паузу' }).click();
  await expect(sheet.locator('[data-role="nudge-state"]')).toHaveText('Автопинок: на паузе');
  await sheet.getByRole('button', { name: 'Возобновить' }).click();
  await expect(sheet.locator('[data-role="nudge-state"]')).toHaveText('Автопинок: следит');

  await sheet.getByRole('button', { name: 'Снять правило' }).click();
  await expect(page.getByText('Снять автопинок?')).toBeVisible();
  await page.getByRole('button', { name: 'Снять автопинок' }).click();
  await expect(sheet).toHaveCount(0);
  await expect(line).toHaveCount(0);
});

test('nudge_input_is_a_quiet_line_and_owner_messages_skip_it', async ({ page }) => {
  await bootChat(page);
  const entry = page.locator('[data-role="nudge-line"]');
  await expect(entry).toContainText(
    'Автопинок: толчок 2 из 3 · нет маркера готовности, обещал продолжить',
  );
  await expect(entry.locator('[data-role="nudge-body"]')).toHaveCount(0);
  await entry.locator('button').click();
  await expect(entry.locator('[data-role="nudge-body"]')).toContainText(
    'Automatic reminder from canterlot',
  );

  await page.getByRole('button', { name: 'Мои сообщения' }).click();
  const list = page.locator('[data-owner-list]');
  await expect(list).toBeVisible();
  await expect(list).toContainText('Обычное сообщение владельца');
  await expect(list).not.toContainText('Automatic reminder');
});
