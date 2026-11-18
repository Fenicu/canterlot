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

async function boot(page: Page, event: Record<string, unknown>) {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 384 : 1280, height: 844 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Сессия' });
  api.histories.set(id, [hungInput(api, event)]);
  await installChatRoutes(page, hub, api, OWNER);
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
  await boot(page, {
    kind: 'turn_hung',
    turn_hung_notice: true,
    session_id: 'a',
    queued_input_id: queued
  });
  const card = page.locator('[data-role="hung-card"]');
  await expect(card).toBeVisible();
  await expect(card).toContainText('Ход завис');
  const steer = card.locator('[data-action="steer"] button');
  const interrupt = card.locator('[data-action="interrupt"] button');
  await expect(steer).toBeVisible();
  await expect(interrupt).toBeVisible();

  let steered = false;
  await page.route('**/api/v1/conversations/*/inputs/*/send-now', async (route) => {
    steered = true;
    await route.fulfill({ json: { requested: { steer_command_id: '00000000-0000-4000-8000-0000000000f1' } } });
  });
  await steer.click();
  await expect(card.locator('[data-role="hung-done"]')).toContainText('Влито в ход');
  expect(steered).toBe(true);

  await interrupt.click();
  await expect(card.locator('[data-role="hung-done"]')).toContainText('Ход прерван');
});

test('without a queued input the card offers interrupt only', async ({ page }) => {
  await boot(page, { kind: 'turn_hung', turn_hung_notice: true, session_id: 'a' });
  const card = page.locator('[data-role="hung-card"]');
  await expect(card.locator('[data-action="steer"]')).toHaveCount(0);
  await expect(card.locator('[data-action="interrupt"] button')).toBeVisible();
});
