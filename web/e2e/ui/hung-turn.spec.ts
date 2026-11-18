import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import type { HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-11-18T12:00:00.000Z';

function hungInput(api: FakeHubApi, event: Record<string, unknown>): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `hung-${api.id()}`,
    state: 'accepted',
    content: [{ type: 'text', text: 'neutral service note' }],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT,
    origin: { kind: 'event', chain_depth: 0, event }
  } as HistoryInput;
}

const EPISODE = '00000000-0000-4000-8000-0000000000c1';
const EPISODE_PATH = '**/api/v1/conversations/*/hung-episodes/*';

async function episodeState(page: Page, state: string) {
  await page.route(EPISODE_PATH, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await route.fulfill({ json: { episode_id: EPISODE, state } });
  });
}

async function boot(page: Page, event: Record<string, unknown>, episode: string | null = null) {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 384 : 1280, height: 844 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Сессия' });
  api.histories.set(id, [hungInput(api, event)]);
  await installChatRoutes(page, hub, api, OWNER);
  // После общих маршрутов: последний зарегистрированный отвечает первым.
  if (episode) await episodeState(page, episode);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { id };
}

test('a rescued hung turn is a quiet feed row', async ({ page }) => {
  await boot(page, { kind: 'turn_hung', turn_hung_action: 'interrupted', session_id: 'a' });
  const row = page.locator('[data-event-kind="turn_hung"]');
  await expect(row).toContainText('Ход завис в генерации, прерван и продолжен');
  await expect(row).not.toContainText('✓');
});

test('the owner hung card offers steer and interrupt', async ({ page }) => {
  const queued = '00000000-0000-4000-8000-0000000000e1';
  await boot(
    page,
    {
      kind: 'turn_hung',
      turn_hung_notice: true,
      turn_hung_episode: EPISODE,
      session_id: 'a',
      queued_input_id: queued
    },
    'open'
  );
  const card = page.locator('[data-role="hung-card"]');
  await expect(card).toBeVisible();
  await expect(card).toContainText('Ход завис');
  const steer = card.locator('[data-action="steer"] button');
  const interrupt = card.locator('[data-action="interrupt"] button');
  await expect(steer).toBeVisible();
  await expect(interrupt).toBeVisible();

  let steered = '';
  await page.route('**/api/v1/conversations/*/hung-episodes/*/steer', async (route) => {
    steered = route.request().url();
    await route.fulfill({ json: { requested: { steer_command_id: '00000000-0000-4000-8000-0000000000f1' } } });
  });
  await steer.click();
  await expect(card.locator('[data-role="hung-done"]')).toContainText('Влито в ход');
  expect(steered).toContain(`/hung-episodes/${EPISODE}/steer`);
  // Действие закрыло эпизод: вторая кнопка больше не действует.
  await expect(interrupt).toBeDisabled();
});

test('without a queued input the card offers interrupt only', async ({ page }) => {
  await boot(
    page,
    { kind: 'turn_hung', turn_hung_notice: true, turn_hung_episode: EPISODE, session_id: 'a' },
    'open'
  );
  const card = page.locator('[data-role="hung-card"]');
  await expect(card.locator('[data-action="steer"]')).toHaveCount(0);
  const interrupt = card.locator('[data-action="interrupt"] button');
  await expect(interrupt).toBeEnabled();
  let hit = '';
  await page.route('**/api/v1/conversations/*/hung-episodes/*/interrupt', async (route) => {
    hit = route.request().url();
    await route.fulfill({ json: { requested: { interrupt_command_id: '00000000-0000-4000-8000-0000000000f2' } } });
  });
  await interrupt.click();
  await expect(card.locator('[data-role="hung-done"]')).toContainText('Ход прерван');
  expect(hit).toContain(`/hung-episodes/${EPISODE}/interrupt`);
});

test('a resolved card is not actionable', async ({ page }) => {
  await boot(
    page,
    {
      kind: 'turn_hung',
      turn_hung_notice: true,
      turn_hung_episode: EPISODE,
      session_id: 'a',
      queued_input_id: '00000000-0000-4000-8000-0000000000e1'
    },
    'turn_ended'
  );
  const card = page.locator('[data-role="hung-card"]');
  await expect(card.locator('[data-role="hung-resolved"]')).toContainText('Ход закончился');
  await expect(card.locator('[data-action="steer"]')).toHaveCount(0);
  await expect(card.locator('[data-action="interrupt"] button')).toBeDisabled();
});

test('a click on a card that was closed meanwhile shows it as resolved', async ({ page }) => {
  await boot(
    page,
    { kind: 'turn_hung', turn_hung_notice: true, turn_hung_episode: EPISODE, session_id: 'a' },
    'open'
  );
  const card = page.locator('[data-role="hung-card"]');
  await page.route('**/api/v1/conversations/*/hung-episodes/*/interrupt', async (route) => {
    await route.fulfill({
      status: 409,
      json: {
        error: {
          code: 'invalid_state',
          message: 'Некорректное состояние',
          scope: 'browser',
          retry: 'never',
          correlation_id: '00000000-0000-4000-8000-0000000000f1',
          details: { reason: 'hung_episode_resolved' }
        }
      }
    });
  });
  await card.locator('[data-action="interrupt"] button').click();
  await expect(card.locator('[data-role="hung-resolved"]')).toBeVisible();
  await expect(card.locator('[data-role="hung-failed"]')).toHaveCount(0);
  await expect(card.locator('[data-action="interrupt"] button')).toBeDisabled();
});

test('a worker without the guarded interrupt refuses the card button with a clear reason', async ({
  page
}) => {
  await boot(
    page,
    { kind: 'turn_hung', turn_hung_notice: true, turn_hung_episode: EPISODE, session_id: 'a' },
    'open'
  );
  const card = page.locator('[data-role="hung-card"]');
  await page.route('**/api/v1/conversations/*/hung-episodes/*/interrupt', async (route) => {
    await route.fulfill({
      status: 409,
      json: {
        error: {
          code: 'capability_missing',
          message: 'Требуется обновление воркера',
          scope: 'delivery',
          retry: 'after_condition',
          correlation_id: '00000000-0000-4000-8000-0000000000f2',
          details: { reason: 'capability_missing' }
        }
      }
    });
  });
  await card.locator('[data-action="interrupt"] button').click();
  await expect(card.locator('[data-role="hung-failed"]')).toContainText('обновите воркер');
  await expect(card.locator('[data-action="interrupt"] button')).toBeEnabled();
});
