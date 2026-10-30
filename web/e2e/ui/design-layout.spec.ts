import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, seed?: (api: FakeHubApi, id: string) => void) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Восстановление потока после сна', model: 'opus-5.5' });
  seed?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function startTurn(hub: FakeHub, api: FakeHubApi, id: string): void {
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'derived_changed', conversation_id: id, payload: { conversation_id: id, activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() }, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: new Date().toISOString(), settings: { model: 'opus-5.5', effort: 'high', permission_mode: 'default' }, crunch: { enabled: false } } });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: api.id() } } } });
  hub.flush();
}

test.describe('поле ввода', () => {
  test('resting_composer_shows_no_error_and_keeps_the_reason_for_screen_readers', async ({ page }) => {
    await boot(page);
    const field = page.getByRole('textbox', { name: 'Сообщение' });
    await expect(page.locator('[data-composer] .reason')).toHaveCount(0);
    await expect(field).toHaveAttribute('placeholder', 'Сообщение агенту…');
    await expect(field).toHaveAccessibleDescription('Введите сообщение или прикрепите файл');
  });

  test('send_stays_on_one_row_with_attach_when_interrupt_appears', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    await page.getByRole('textbox', { name: 'Сообщение' }).fill('уточнение');
    const send = page.getByRole('button', { name: 'Отправить', exact: true });
    const before = await send.boundingBox();
    startTurn(hub, api, id);
    await expect(page.getByRole('button', { name: 'Прервать' })).toBeVisible();
    const after = await send.boundingBox();
    const attach = await page.getByRole('button', { name: 'Прикрепить файл' }).boundingBox();
    expect(after!.x).toBeCloseTo(before!.x, 0);
    expect(after!.y).toBeCloseTo(before!.y, 0);
    expect(attach!.y).toBeCloseTo(after!.y, 0);
  });

  test('field_grows_up_to_six_lines_and_the_capsule_carries_the_focus_ring', async ({ page }) => {
    await boot(page);
    const field = page.getByRole('textbox', { name: 'Сообщение' });
    const empty = (await field.boundingBox())!.height;
    await field.fill(['раз', 'два', 'три'].join('\n'));
    await expect.poll(async () => (await field.boundingBox())!.height).toBeGreaterThan(empty * 2);
    await field.fill(Array.from({ length: 14 }, (_, index) => `строка ${index}`).join('\n'));
    await expect.poll(async () => (await field.boundingBox())!.height).toBeGreaterThan(empty * 5);
    const capped = (await field.boundingBox())!.height;
    expect(capped).toBeLessThanOrEqual(empty * 6 + 1);
    await field.focus();
    const ring = await page.locator('.capsule').evaluate((node) => getComputedStyle(node).outlineStyle);
    expect(ring).toBe('solid');
    expect(await field.evaluate((node) => getComputedStyle(node).outlineStyle)).toBe('none');
  });
});
