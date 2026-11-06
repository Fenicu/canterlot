import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

async function boot(page: Page, opts: { tokens?: number } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  api.pushContext(id, opts.tokens ?? 630_000, 1_000_000);
  // Исполнение живо: в шапке есть квадрат остановки сессии.
  (api.details.get(id) as Record<string, unknown>).execution = { state: 'running', generation: 1 };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  return { hub, api, id };
}

/** Ход идёт и агент пишет ответ: под ответом живая строка хода. */
function startTurn(hub: FakeHub, api: FakeHubApi, id: string, opts: { answer?: boolean; phase?: string } = {}): void {
  const scope = `conversation:${id}`;
  const inputId = api.id();
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: opts.phase ? { phase: opts.phase } : {}, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } } });
  if (opts.answer !== false) {
    const blockId = api.id();
    const opened = hub.append(scope, {
      kind: 'block_started',
      conversation_id: id,
      payload: { block_id: blockId, rev: 0, kind: 'text', input_command_id: inputId, parent_block_id: null, agent: 'агент', model: 'opus', tool_name: null, summary: null, approved_by: null }
    });
    hub.append(scope, { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'Смотрю файлы проекта' } });
    if (!api.histories.has(id)) api.histories.set(id, []);
    api.histories.get(id)!.push({
      kind: 'block', id: blockId, block_kind: 'text', content: 'Смотрю файлы проекта', content_bytes: 37, content_truncated: false,
      created_seq: opened.scope_seq, created_at: AT, rev: 1, state: 'streaming', agent: 'агент', model: 'opus', tool_name: null, summary: null,
      parent_block_id: null, input_command_id: inputId, line_count: 1
    });
    api.blocks.set(blockId, { text: 'Смотрю файлы проекта', rev: 1, state: 'streaming' });
  }
  hub.flush();
}

/** Текст в строке композера, который режется многоточием или вылезает из своей коробки. */
async function truncated(page: Page): Promise<string[]> {
  return page.locator('[data-composer]').evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>('*')]
      .filter((node) => !node.classList.contains('sr-only') && (node.textContent ?? '').trim() !== '' && node.getClientRects().length > 0 && (getComputedStyle(node).textOverflow === 'ellipsis' || getComputedStyle(node).whiteSpace === 'nowrap') && node.scrollWidth > node.clientWidth + 0.5)
      .map((node) => `${node.className}: ${node.textContent?.trim()} ${node.scrollWidth}/${node.clientWidth} parent ${node.parentElement!.getBoundingClientRect().width}`)
  );
}

async function watch(page: Page, pattern: string): Promise<string[]> {
  const hits: string[] = [];
  await page.route(pattern, async (route) => {
    if (route.request().method() === 'POST') hits.push(route.request().headers()['idempotency-key'] ?? '');
    await route.fallback();
  });
  return hits;
}

function activity(hub: FakeHub, id: string, state: string, phase: string): void {
  hub.append(`conversation:${id}`, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state, evidence: { phase }, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.flush();
}

const liveStop = (page: Page) => page.locator('[data-working] [data-interrupt]');

test('the_composer_row_has_no_interrupt_and_the_live_line_interrupts_the_turn', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/interrupt');
  await expect(liveStop(page)).toHaveCount(0);
  startTurn(hub, api, id);
  await expect(page.locator('[data-working]')).toBeVisible();
  // Рядом с «Отправить» прерывания нет: промах по отправке не прерывает ход.
  await expect(page.locator('[data-composer]').getByRole('button', { name: /Прерв/ })).toHaveCount(0);
  const stop = page.locator('[data-working]').getByRole('button', { name: 'Прервать' });
  await expect(stop).toBeVisible();
  const box = (await stop.boundingBox())!;
  expect(box.height).toBeGreaterThanOrEqual(44);
  const send = (await page.getByRole('button', { name: 'Отправить', exact: true }).boundingBox())!;
  expect(send.y - (box.y + box.height), 'interrupt is far from send').toBeGreaterThan(44);
  await stop.click();
  await expect(page.locator('[data-working]').getByRole('button', { name: 'Прерываю…' })).toBeVisible();
  // Повторное нажатие, пока ждём машину, второго прерывания не шлёт.
  await page.locator('[data-working]').getByRole('button', { name: 'Прерываю…' }).click();
  await expect.poll(() => hits.length).toBe(1);
  // Тот же ключ и в меню: пункт показывает, что прерывание уже идёт.
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await expect(page.getByRole('menuitem', { name: 'Прерываю…' })).toBeVisible();
});

test('the_session_menu_interrupts_a_turn_without_an_answer_yet', async ({ page }) => {
  const { hub, id } = await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/interrupt');
  hub.append(`conversation:${id}`, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: 'free' } });
  activity(hub, id, 'working', 'starting');
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await expect(page.getByRole('menuitem', { name: 'Прервать ход' })).toHaveCount(0);
  await page.keyboard.press('Escape');
  // Ход пробуждения не держит слот очереди, но модель уже работает.
  activity(hub, id, 'working', 'generating');
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Прервать ход' }).click();
  await expect.poll(() => hits.length).toBe(1);
  await expect(page.getByRole('menu')).toHaveCount(0);
});

test('the_header_stop_button_still_stops_the_session', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const interrupts = await watch(page, '**/api/v1/conversations/*/interrupt');
  const stops = await watch(page, '**/api/v1/conversations/*/stop');
  startTurn(hub, api, id);
  await page.locator('.chat-header').getByRole('button', { name: 'Остановить сессию' }).click();
  await expect.poll(() => stops.length).toBe(1);
  expect(interrupts).toHaveLength(0);
});

test('interrupt_without_a_live_execution_does_not_hang_on_progress', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  await page.route('**/api/v1/conversations/*/interrupt', async (route) => {
    await route.fulfill({ json: { requested: { interrupt_command_id: null, withdraw_command_id: null } } });
  });
  startTurn(hub, api, id);
  activity(hub, id, 'stalled', 'generating');
  await liveStop(page).click();
  await expect(page.locator('[data-working]').getByRole('button', { name: 'Прерывание не требуется' })).toBeVisible();
});

test('interrupt_label_ends_on_every_terminal_status_with_its_own_text', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  startTurn(hub, api, id);
  await liveStop(page).click();
  await expect(page.locator('[data-working]').getByRole('button', { name: 'Прерываю…' })).toBeVisible();
  const endings = [
    ['completed', 'Прервано'],
    ['not_applicable', 'Прерывание не требуется'],
    ['superseded', 'Прерывание заменено'],
    ['rejected_stale', 'Прерывание устарело']
  ] as const;
  for (const [status, text] of endings) {
    hub.append(`conversation:${id}`, { kind: 'interrupt_state_changed', conversation_id: id, payload: { command_id: api.id(), status } });
    hub.flush();
    await expect(page.locator('[data-working]').getByRole('button', { name: text })).toBeVisible();
  }
});

/** Строка композера не режет текст ни в каком состоянии хода и контекста. */
for (const width of [320, 360, 390]) {
  test(`the_composer_row_cuts_no_text_at_${width}px`, async ({ page }, info) => {
    test.skip(info.project.name !== 'phone', 'ширины телефона');
    await page.setViewportSize({ width, height: 844 });
    const { hub, api, id } = await boot(page, { tokens: 960_000 });
    const indicator = page.getByRole('button', { name: /^Контекст/ });
    await expect(indicator).toBeVisible();
    expect(await truncated(page), 'idle').toEqual([]);
    startTurn(hub, api, id);
    await expect(page.locator('[data-working]')).toBeVisible();
    expect(await truncated(page), 'running').toEqual([]);
    await expect(indicator).toContainText('96');
    // Сжатие показывает в индикаторе свою строку: она тоже помещается.
    hub.append(`conversation:${id}`, { kind: 'phase_changed', conversation_id: id, payload: { phase: 'context_maintenance', tool_name: null, summary: null } });
    hub.flush();
    await expect(indicator).toContainText('Сжимает контекст');
    expect(await truncated(page), 'compacting').toEqual([]);
    // С вложением в лотке строка та же.
    await page.locator('[data-composer] input[type="file"]').setInputFiles({ name: 'shot.png', mimeType: 'image/png', buffer: Buffer.from([137, 80, 78, 71]) });
    await expect(page.locator('[data-composer] .tray')).toBeVisible();
    expect(await truncated(page), 'with attachment').toEqual([]);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });
}
