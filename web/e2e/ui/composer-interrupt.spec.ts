import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

async function boot(page: Page, opts: { tokens?: number; history?: number } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  api.pushContext(id, opts.tokens ?? 630_000, 1_000_000);
  if (opts.history) {
    const items = seedHistory(api, id, opts.history);
    // Номера засеянной истории заняты: первое живое событие встаёт после неё.
    for (let index = 0; index < items.length; index++) items[index]!.created_seq = String(index - items.length);
    api.bindConversation(id);
  }
  // Исполнение живо: в шапке есть квадрат остановки сессии.
  (api.details.get(id) as Record<string, unknown>).execution = { state: 'running', generation: 1 };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  return { hub, api, id };
}

/** Ответ текущего хода: блок, дельта и запись в истории, как их присылает настоящий хаб. */
function answer(hub: FakeHub, api: FakeHubApi, id: string, inputId: string): string {
  const scope = `conversation:${id}`;
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
  return blockId;
}

/** Ход идёт: по умолчанию агент уже пишет ответ, без ответа остаётся только живая строка. */
function startTurn(hub: FakeHub, api: FakeHubApi, id: string, opts: { answer?: boolean; phase?: string } = {}): { inputId: string; blockId: string | null } {
  const scope = `conversation:${id}`;
  const inputId = api.id();
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: opts.phase ? { phase: opts.phase } : {}, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } } });
  const blockId = opts.answer === false ? null : answer(hub, api, id, inputId);
  hub.flush();
  return { inputId, blockId };
}

/** Ход закончился: очередь на паузе, активность снова свободна. */
function finishTurn(hub: FakeHub, api: FakeHubApi, id: string, status = 'completed'): void {
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'interrupt_state_changed', conversation_id: id, payload: { command_id: api.id(), status } });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [{ reason: 'user_stop' }], hold_reasons: [], run_slot: 'free' } });
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'idle', evidence: {}, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [{ reason: 'user_stop' }], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
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
      activity: { state, evidence: { phase }, since: new Date().toISOString() },
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
  const line = page.locator('[data-role="live-turn"]');
  await expect(line.getByRole('button', { name: 'Прервать' })).toBeVisible();
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Прервать ход' }).click();
  await expect.poll(() => hits.length).toBe(1);
  await expect(page.getByRole('menu')).toHaveCount(0);
  // Пункт меню и кнопка строки делят состояние: второй запрос не уходит.
  await expect(line.getByRole('button', { name: 'Прерываю…' })).toBeVisible();
  await line.getByRole('button', { name: 'Прерываю…' }).click();
  await page.waitForTimeout(200);
  expect(hits).toHaveLength(1);
});

test('the_live_line_starts_the_turn_before_the_first_answer_and_shows_the_outcome', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/interrupt');
  startTurn(hub, api, id, { answer: false, phase: 'starting' });
  const line = page.locator('[data-role="live-turn"]');
  await expect(line).toBeVisible();
  await expect(line.locator('.live[data-working]')).toBeVisible();
  await expect(line).toContainText('Запускается');
  await expect(line.getByRole('button', { name: 'Прервать' })).toBeVisible();
  // Идёт ход, а не пустой разговор: подсказки «Сообщений пока нет» нет.
  await expect(page.locator('[data-feed-empty]')).toHaveCount(0);
  await line.getByRole('button', { name: 'Прервать' }).click();
  await expect(line.getByRole('button', { name: 'Прерываю…' })).toBeVisible();
  await expect.poll(() => hits.length).toBe(1);
  finishTurn(hub, api, id);
  const outcome = page.locator('[data-role="interrupt-outcome"]');
  await expect(outcome).toHaveText('Прервано');
  await expect(page.locator('[data-feed-empty]')).toHaveCount(0);
  await expect(line).toHaveCount(0);
});

test('the_live_line_attaches_to_the_answer_without_a_second_one', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const { inputId } = startTurn(hub, api, id, { answer: false });
  await expect(page.locator('[data-role="live-turn"]')).toBeVisible();
  answer(hub, api, id, inputId);
  hub.flush();
  const answerLine = page.locator('[data-kind="answer"] .live[data-working]');
  await expect(answerLine).toHaveCount(1);
  await expect(page.locator('[data-role="live-turn"]')).toHaveCount(0);
  // В ленте ровно одна живая строка, и она стоит в карточке ответа.
  await expect(page.locator('.feed .live[data-working]')).toHaveCount(1);
});

test('a_message_after_a_long_idle_counts_the_new_turn_not_the_old_phase', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const scope = `conversation:${id}`;
  // Прошлый ход оставил отметку фазы много часов назад; сама сессия свободна.
  const stale = new Date(Date.now() - 7 * 3_600_000).toISOString();
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'idle', evidence: { last_turn_at: stale }, since: stale },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: stale,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.flush();
  // Владелец пишет сообщение.
  await page.getByRole('textbox', { name: 'Сообщение' }).fill('а что ты предлагаешь?');
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => hits.length).toBe(1);
  // Хаб принял ход: состояние работы началось только что, а отметка фазы — из прошлого хода.
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: { phase: 'generating', phase_since: stale }, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: stale,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: api.id() } } } });
  hub.flush();
  const line = page.locator('[data-role="live-turn"]');
  await expect(line).toBeVisible();
  // Часы строки начинаются с нового хода: секунды, а не часы прошлой фазы.
  await expect(line).toContainText(/Генерирует · \d+ с/);
  await expect(line).not.toContainText('ч');
});

test('a_wake_up_turn_without_an_answer_shows_the_live_line', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/interrupt');
  hub.append(`conversation:${id}`, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: 'free' } });
  activity(hub, id, 'working', 'generating');
  const line = page.locator('[data-role="live-turn"]');
  await expect(line).toBeVisible();
  await expect(line).toContainText('Генерирует');
  await line.getByRole('button', { name: 'Прервать' }).click();
  await expect.poll(() => hits.length).toBe(1);
  finishTurn(hub, api, id);
  await expect(page.locator('[data-role="interrupt-outcome"]')).toHaveText('Прервано');
});

test('the_early_live_line_keeps_the_feed_pinned_to_the_bottom', async ({ page }) => {
  const { hub, api, id } = await boot(page, { history: 40 });
  const feed = page.locator('.feed');
  const gap = () => feed.evaluate((node) => node.scrollHeight - node.clientHeight - node.scrollTop);
  await expect(feed).toHaveAttribute('data-pinned', '');
  await expect.poll(gap).toBeLessThanOrEqual(2);
  const { inputId } = startTurn(hub, api, id, { answer: false, phase: 'starting' });
  await expect(page.locator('[data-role="live-turn"]')).toBeVisible();
  await expect(feed).toHaveAttribute('data-pinned', '');
  await expect.poll(gap).toBeLessThanOrEqual(2);
  // Строка уходит в карточку ответа: лента остаётся прижатой и на этом переходе.
  answer(hub, api, id, inputId);
  hub.flush();
  await expect(page.locator('[data-role="live-turn"]')).toHaveCount(0);
  await expect(page.locator('[data-kind="answer"] .live[data-working]')).toHaveCount(1);
  await expect(feed).toHaveAttribute('data-pinned', '');
  await expect.poll(gap).toBeLessThanOrEqual(2);
});

for (const motion of ['full', 'reduced'] as const) {
  test(`the_early_live_line_motif_${motion}`, async ({ page }) => {
    if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
    const { hub, api, id } = await boot(page);
    startTurn(hub, api, id, { answer: false, phase: 'starting' });
    const line = page.locator('[data-role="live-turn"]');
    await expect(line).toBeVisible();
    const names = await line.locator('.write i').evaluateAll((nodes) =>
      nodes.map((node) => getComputedStyle(node).animationName)
    );
    if (motion === 'reduced') expect(names).toEqual(['none', 'none', 'none']);
    else expect(names.every((name) => /work-write-(first|second|third)$/.test(name))).toBe(true);
  });
}

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
    await expect.poll(() => truncated(page), 'idle').toEqual([]);
    startTurn(hub, api, id);
    await expect(page.locator('[data-working]')).toBeVisible();
    await expect.poll(() => truncated(page), 'running').toEqual([]);
    await expect(indicator).toContainText('96');
    // Сжатие показывает в индикаторе свою строку: она тоже помещается.
    hub.append(`conversation:${id}`, { kind: 'phase_changed', conversation_id: id, payload: { phase: 'context_maintenance', tool_name: null, summary: null } });
    hub.flush();
    await expect(indicator).toContainText('Сжимает контекст');
    await expect.poll(() => truncated(page), 'compacting').toEqual([]);
    // С вложением в лотке строка та же.
    await page.locator('[data-composer] input[type="file"]').setInputFiles({ name: 'shot.png', mimeType: 'image/png', buffer: Buffer.from([137, 80, 78, 71]) });
    await expect(page.locator('[data-composer] .tray')).toBeVisible();
    await expect.poll(() => truncated(page), 'with attachment').toEqual([]);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });
}
