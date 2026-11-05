import { mkdtempSync, rmSync, truncateSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

async function boot(page: Page, opts: { probe?: boolean; count?: number } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  if (opts.count) seedHistory(api, id, opts.count);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}${opts.probe ? '?probe=1' : ''}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  return { hub, api, id };
}

async function watch(page: Page, pattern: string) {
  const hits: { key: string; url: string; body: unknown }[] = [];
  await page.route(pattern, async (route) => {
    if (route.request().method() === 'POST') {
      let body: unknown = null;
      try {
        body = route.request().postDataJSON();
      } catch {
        body = null;
      }
      hits.push({ key: route.request().headers()['idempotency-key'] ?? '', url: route.request().url(), body });
    }
    await route.fallback();
  });
  return hits;
}

function started(blockId: string) {
  return {
    block_id: blockId,
    rev: 0,
    kind: 'text' as const,
    input_command_id: null,
    parent_block_id: null,
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    approved_by: null
  };
}

test('enter_on_touch_inserts_newline_and_sends_nothing', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'pointer: coarse');
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('сенсор');
  await area.press('Enter');
  await expect(area).toHaveValue(/сенсор\n/);
  await page.waitForTimeout(250);
  expect(hits).toHaveLength(0);
});

test('textarea_has_enterkeyhint_enter_and_no_form_submission_path', async ({ page }) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(area).toHaveAttribute('enterkeyhint', 'enter');
  await expect(page.locator('form')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Отправить', exact: true })).toHaveAttribute('type', 'button');
});

test('enter_hint_is_shown_on_desktop_and_hidden_on_touch', async ({ page }, info) => {
  await boot(page);
  const hint = page.getByText('Enter: отправить · Shift+Enter: новая строка');
  if (info.project.name === 'desktop') await expect(hint).toBeVisible();
  else await expect(hint).toHaveCount(0);
});

test('desktop_enter_sends_and_shift_enter_inserts_newline', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'мышь');
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('строка');
  await area.press('Shift+Enter');
  await expect(area).toHaveValue(/строка\n/);
  expect(hits).toHaveLength(0);
  await area.press('Enter');
  await expect.poll(() => hits.length).toBe(1);
});

test('ctrl_shift_enter_opens_send_later', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/**');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('позже');
  await area.press('Control+Shift+Enter');
  await expect(page.getByRole('dialog', { name: 'Отправить позже' })).toBeVisible();
  expect(hits).toHaveLength(0);
});

test('send_later_closes_outside_keeps_inside_and_toggles_on_its_button', async ({ page, isMobile }) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('позже');
  await area.press('Control+Shift+Enter');
  const panel = page.getByRole('dialog', { name: 'Отправить позже' });
  await expect(panel).toBeVisible();
  await panel.getByRole('heading', { name: 'Отправить позже' }).click();
  await expect(panel).toBeVisible();
  if (isMobile) {
    await page.touchscreen.tap(195, 12);
    await expect(panel).toHaveCount(0);
    return;
  }
  await page.locator('.feed').click({ position: { x: 8, y: 8 } });
  await expect(panel).toHaveCount(0);
  const clock = page.getByRole('button', { name: 'Отправить позже' });
  await clock.click();
  await expect(panel).toBeVisible();
  await clock.click();
  await expect(panel).toHaveCount(0);
  await clock.click();
  await page.keyboard.press('Escape');
  await expect(panel).toHaveCount(0);
});

test('ime_composition_enter_never_sends_in_the_browser', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('иероглиф');
  await area.evaluate((el) => {
    el.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    el.dispatchEvent(
      new KeyboardEvent('keydown', {
        key: 'Enter',
        code: 'Enter',
        keyCode: 229,
        isComposing: true,
        bubbles: true,
        cancelable: true
      })
    );
    el.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true }));
  });
  await page.waitForTimeout(250);
  expect(hits).toHaveLength(0);
  await expect(area).toHaveValue('иероглиф');
});

test('large_paste_never_sends_and_over_limit_disables_send_with_reason', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  const text = 'a'.repeat(300 * 1024);
  await area.evaluate((el, value) => {
    const field = el as HTMLTextAreaElement;
    field.value = value;
    field.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertFromPaste', data: value }));
  }, text);
  await expect(page.getByText('Текст длиннее 256 КиБ')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Отправить', exact: true })).toHaveAttribute('aria-disabled', 'true');
  await area.press('End');
  await area.press('x');
  await expect(area).toHaveValue(/x$/);
  await page.waitForTimeout(250);
  expect(hits).toHaveLength(0);
});

test('send_happens_only_via_the_send_button_tap', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'на столе Enter отправляет');
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('кнопка');
  await area.press('Enter');
  await page.waitForTimeout(200);
  expect(hits).toHaveLength(0);
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => hits.length).toBe(1);
});

test('double_tap_on_send_creates_one_operation', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('два');
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  await send.click();
  await page.waitForTimeout(80);
  await send.click({ force: true });
  await page.waitForTimeout(300);
  expect(hits).toHaveLength(1);
});

test('long_press_on_send_opens_the_schedule_sheet_and_sends_nothing', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/**');
  await page.getByRole('textbox', { name: 'Сообщение' }).fill('удержание');
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  const box = await send.boundingBox();
  if (!box) throw new Error('send_missing');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.waitForTimeout(650);
  await page.mouse.up();
  await expect(page.getByRole('dialog', { name: 'Отправить позже' })).toBeVisible();
  expect(hits).toHaveLength(0);
});

test('send_button_keeps_focus_in_the_textarea_so_the_keyboard_stays_open', async ({ page }) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.click();
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  const box = await send.boundingBox();
  if (!box) throw new Error('send_missing');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await expect(area).toBeFocused();
  await page.mouse.up();
  await expect(area).toBeFocused();
});

test('empty_draft_send_is_disabled_and_the_reason_stays_out_of_sight', async ({ page }) => {
  await boot(page);
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toHaveAccessibleDescription('Введите сообщение или прикрепите файл');
  await expect(page.locator('[data-composer] .reason')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Отправить', exact: true })).toHaveAttribute('aria-disabled', 'true');
});

test('send_is_disabled_while_attachments_upload_with_the_count_reason', async ({ page }) => {
  await boot(page);
  await page.route('**/api/v1/attachments', async (route) => {
    if (route.request().method() === 'POST') await new Promise(() => undefined);
    else await route.fallback();
  });
  await page.setInputFiles('input[type=file]', {
    name: 'a.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('abc')
  });
  await page.getByRole('textbox', { name: 'Сообщение' }).fill('с файлом');
  await expect(page.getByText('Файлы ещё загружаются (1 из 1)')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Отправить', exact: true })).toHaveAttribute('aria-disabled', 'true');
  await page.getByRole('textbox', { name: 'Сообщение' }).press('Control+Shift+Enter');
  await expect(page.getByRole('button', { name: /Запланировать/ })).toBeDisabled();
  await expect(page.getByText('Станет доступно, когда загрузятся файлы')).toBeVisible();
});

test('a_failed_attachment_blocks_the_send_until_it_is_retried_or_removed', async ({ page }) => {
  await boot(page);
  await page.route('**/api/v1/attachments', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    await route.fulfill({ status: 422, json: { error: { code: 'attachment_unsupported', message: 'Отказ', scope: 'hub_db', retry: 'never', correlation_id: '00000000-0000-4000-8000-0000000000e5' } } });
  });
  await page.setInputFiles('input[type=file]', { name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('abc') });
  await page.getByRole('textbox', { name: 'Сообщение' }).fill('с файлом');
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  // Хеш считается в воркере: под нагрузкой до запроса хабу проходит не одна секунда.
  await expect(page.getByText('Файл не загрузился: повторите или уберите его')).toBeVisible({ timeout: 30_000 });
  await expect(send).toHaveAttribute('aria-disabled', 'true');
  await page.locator('[data-composer] .tray').getByRole('button', { name: 'Убрать' }).click();
  await expect(send).toHaveAttribute('aria-disabled', 'false');
});

test('p01_probe_marks_press_and_local_item_paint', async ({ page }) => {
  await boot(page, { probe: true });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('зонд');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect(page.getByText('зонд')).toBeVisible();
  const marks = await page.evaluate(() => {
    const probe = (window as unknown as { __canterlot_probe?: { measure(name: string, from: string): number | null } }).__canterlot_probe;
    return { pressed: probe?.measure('press', 'send_pressed') ?? null, painted: probe?.measure('paint', 'input_painted') ?? null };
  });
  expect(marks.pressed).not.toBeNull();
  expect(marks.painted).not.toBeNull();
});

test('typing_does_not_re_render_the_timeline_while_streaming', async ({ page }) => {
  const { hub, api, id } = await boot(page, { probe: true, count: 2 });
  const blockId = api.id();
  hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: started(blockId) });
  hub.flush();
  const feed = page.locator('.feed');
  await expect.poll(async () => Number(await feed.getAttribute('data-renders'))).toBeGreaterThan(0);
  await page.waitForTimeout(100);
  const before = Number(await feed.getAttribute('data-renders'));
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.click();
  await area.pressSequentially('а'.repeat(100), { delay: 5 });
  await page.waitForTimeout(50);
  expect(Number(await feed.getAttribute('data-renders')) - before).toBe(0);
  const counters = await page.evaluate(() => {
    const probe = (window as unknown as { __canterlot_probe?: { snapshot(): { counters?: Record<string, number> } } }).__canterlot_probe;
    return probe?.snapshot().counters ?? {};
  });
  expect(counters.timeline_render ?? 0).toBeGreaterThan(0);
});

test('interrupt_pill_shows_only_while_a_turn_runs_and_reports_progress', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/interrupt');
  await expect(page.getByRole('button', { name: 'Прервать' })).toHaveCount(0);
  const inputId = api.id();
  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } }
  });
  hub.flush();
  const pill = page.getByRole('button', { name: 'Прервать' });
  await expect(pill).toBeVisible();
  await pill.click();
  const busy = page.getByRole('button', { name: 'Прерываю…' });
  await expect(busy).toBeVisible();
  await busy.click();
  await expect.poll(() => hits.length).toBe(1);
});

test('interrupt_without_a_live_execution_does_not_hang_on_progress', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  await page.route('**/api/v1/conversations/*/interrupt', async (route) => {
    await route.fulfill({ json: { requested: { interrupt_command_id: null, withdraw_command_id: null } } });
  });
  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'stalled', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: api.id() } } } });
  hub.flush();
  await page.getByRole('button', { name: 'Прервать' }).click();
  await expect(page.getByRole('button', { name: 'Прерывание не требуется' })).toBeVisible();
});

test('interrupt_pill_shows_during_a_wake_turn_that_holds_no_slot', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  void api;
  const hits = await watch(page, '**/api/v1/conversations/*/interrupt');
  const scope = `conversation:${id}`;
  const activity = (state: string, phase: string) => ({
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
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: 'free' } });
  hub.append(scope, activity('working', 'starting'));
  hub.flush();
  const pill = page.getByRole('button', { name: 'Прервать' });
  await expect(pill).toHaveCount(0);
  hub.append(scope, activity('working', 'generating'));
  hub.flush();
  await expect(pill).toBeVisible();
  hub.append(scope, activity('stalled', 'generating'));
  hub.flush();
  await expect(pill).toBeVisible();
  await pill.click();
  await expect.poll(() => hits.length).toBe(1);
});

test('interrupt_label_ends_on_every_terminal_status_with_its_own_text', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const scope = `conversation:${id}`;
  const inputId = api.id();
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } }
  });
  hub.flush();
  await page.getByRole('button', { name: 'Прервать' }).click();
  await expect(page.getByRole('button', { name: 'Прерываю…' })).toBeVisible();
  const endings = [
    ['completed', 'Прервано'],
    ['not_applicable', 'Прерывание не требуется'],
    ['superseded', 'Прерывание заменено'],
    ['rejected_stale', 'Прерывание устарело']
  ] as const;
  for (const [status, text] of endings) {
    hub.append(scope, {
      kind: 'interrupt_state_changed',
      conversation_id: id,
      payload: { command_id: api.id(), status }
    });
    hub.flush();
    await expect(page.getByRole('button', { name: text })).toBeVisible();
  }
});

test('draft_restores_per_conversation_after_reload', async ({ page }) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('черновик сессии');
  await page.waitForTimeout(400);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toHaveValue('черновик сессии');
});

test('send_works_while_wss_is_broken_and_http_is_up', async ({ page }) => {
  const { hub } = await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  hub.chaos.cut();
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('без сокета');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => hits.length).toBe(1);
  await expect(area).toHaveValue('');
});

test('queue_and_scheduled_chips_have_cancel_and_edit_actions', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const scope = `conversation:${id}`;
  const inputId = api.id();
  hub.append(scope, {
    kind: 'input_queued',
    conversation_id: id,
    payload: {
      input_id: inputId,
      input_seq: '1',
      idempotency_key: api.id(),
      state: 'queued',
      content: [{ type: 'text', text: 'в очереди' }]
    }
  });
  const scheduledId = api.id();
  hub.append(scope, {
    kind: 'scheduled_changed',
    conversation_id: id,
    payload: {
      id: scheduledId,
      conversation_id: id,
      run_at: '2026-10-29T18:00:00.000Z',
      content: [{ type: 'text', text: 'вечером' }],
      attachment_ids: [],
      state: 'pending',
      input_id: null,
      attempts: 0
    }
  });
  hub.flush();
  await expect(page.getByText('в очереди', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Отменить' }).first().click();
  const chip = page.locator('.strip div').filter({ hasText: 'вечером' });
  await chip.getByRole('button', { name: 'Изменить' }).click();
  await expect(page.getByRole('dialog', { name: 'Изменить отправку' })).toBeVisible();
  await page.getByRole('button', { name: 'Закрыть' }).click();
  await page.route('**/api/v1/scheduled/**', async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback();
    await route.fulfill({
      status: 409,
      headers: { 'X-Server-Time-Ms': String(Date.now()) },
      json: {
        error: {
          code: 'not_pending',
          message: 'Уже обработано',
          scope: 'delivery',
          retry: 'never',
          correlation_id: '00000000-0000-4000-8000-0000000000e2'
        }
      }
    });
  });
  await chip.getByRole('button', { name: 'Отменить' }).click();
  await expect(page.getByText('Уже отправлено')).toBeVisible();
});

test('send_later_posts_a_scheduled_message_with_a_key', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/scheduled');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('через час');
  await area.press('Control+Shift+Enter');
  await page.getByRole('radio', { name: /Через 15 минут/ }).check();
  await page.getByRole('button', { name: /Запланировать/ }).click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0]?.url).toContain('/scheduled');
  expect(hits[0]?.key).not.toBe('');
  expect((hits[0]?.body as { run_at?: string })?.run_at).toBeTruthy();
});

test('attachment_tray_shows_progress_errors_and_limits', async ({ page }) => {
  await boot(page);
  const files = Array.from({ length: 11 }, (_, index) => ({
    name: `f${index}.txt`,
    mimeType: 'text/plain',
    buffer: Buffer.from('x')
  }));
  await page.setInputFiles('input[type=file]', files);
  await expect(page.getByText('Не более 10 файлов в одном сообщении')).toBeVisible();
  await expect(page.getByRole('progressbar').first()).toBeVisible();
  await page.setInputFiles('input[type=file]', {
    name: 'empty.txt',
    mimeType: 'text/plain',
    buffer: Buffer.alloc(0)
  });
  await expect(page.getByText('Файл пустой')).toBeVisible();
  const dir = mkdtempSync(join(tmpdir(), 'canterlot-attach-'));
  try {
    const over = join(dir, 'over.bin');
    writeFileSync(over, '');
    truncateSync(over, 104_857_601);
    await page.setInputFiles('input[type=file]', over);
    await expect(page.getByText('Файл больше 100 МиБ')).toBeVisible();
    await expect(page.getByText('over.bin')).toHaveCount(0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('reload_between_press_and_response_does_not_duplicate_the_input', async ({ page }) => {
  await boot(page);
  const keys: string[] = [];
  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const key = route.request().headers()['idempotency-key'] ?? '';
    keys.push(key);
    if (keys.length === 1) {
      await route.abort('failed');
      return;
    }
    await route.fallback();
  });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('один');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => keys.length).toBe(1);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => keys.length).toBe(2);
  expect(keys[0]).toBe(keys[1]);
  expect(keys[0]).not.toBe('');
});

test('composer_is_usable_at_360px_and_200_percent_text', async ({ page }) => {
  await boot(page);
  await page.setViewportSize({ width: 360, height: 640 });
  await page.evaluate(() => {
    document.documentElement.style.fontSize = '32px';
  });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  await expect(area).toBeVisible();
  await expect(send).toBeVisible();
  await area.fill('видно');
  const box = await send.boundingBox();
  expect(box).not.toBeNull();
  expect((box?.x ?? 0) + (box?.width ?? 0)).toBeLessThanOrEqual(360);
  const composer = await page.locator('[data-composer]').boundingBox();
  expect((composer?.width ?? 999)).toBeLessThanOrEqual(360);
});

test('sent_attachment_leaves_the_tray_and_is_not_resent_or_deleted', async ({ page }) => {
  await boot(page);
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const deletes: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'DELETE' && request.url().includes('/api/v1/attachments/')) deletes.push(request.url());
  });
  await page.setInputFiles('input[type=file]', { name: 'отчёт.txt', mimeType: 'text/plain', buffer: Buffer.from('данные') });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  const tray = page.locator('[data-composer] .tray');
  await expect(tray.getByText('отчёт.txt')).toBeVisible();
  await area.fill('с файлом');
  await expect(send).toHaveAttribute('aria-disabled', 'false');
  await send.click();
  await expect.poll(() => hits.length).toBe(1);
  expect((hits[0]?.body as { attachment_ids?: string[] }).attachment_ids).toHaveLength(1);
  await expect(tray).toHaveCount(0);
  await expect(send).toHaveAttribute('aria-disabled', 'true');
  await area.fill('без файла');
  await send.click();
  await expect.poll(() => hits.length).toBe(2);
  expect((hits[1]?.body as { attachment_ids?: string[] }).attachment_ids).toBeUndefined();
  expect(deletes).toEqual([]);
});

async function advance(hub: FakeHub, id: string, key: string, inputId: string, states: { state: string; seq: string }[]) {
  const scope = `conversation:${id}`;
  // Номера событий хаба идут после всей загруженной истории, как у настоящего хаба.
  for (let step = 0; step < 70; step++) hub.append(scope, { kind: 'filler', payload: {} });
  hub.append(scope, {
    kind: 'input_queued',
    conversation_id: id,
    payload: { input_id: inputId, input_seq: '100', idempotency_key: key, state: 'queued', content: [{ type: 'text', text: 'сразу в ленту' }] }
  });
  for (const step of states)
    hub.append(scope, { kind: 'input_state_changed', conversation_id: id, payload: { input_id: inputId, state: step.state } });
  hub.flush();
}

test('send_on_an_idle_session_puts_the_bubble_into_the_feed_without_a_chip_or_a_pill', async ({ page }) => {
  const { hub, api, id } = await boot(page, { count: 12 });
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  const pill = page.getByRole('button', { name: 'К новым сообщениям' });
  await area.fill('сразу в ленту: по поводу того, как смотреть лимиты, есть CodexBar, текст на несколько строк');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  const bubble = page.locator('.bubble', { hasText: 'сразу в ленту' });
  await expect(bubble).toBeVisible();
  await expect(bubble).toContainText('Сохраняется');
  await expect(page.locator('.strip .item')).toHaveCount(0);
  await expect(pill).toHaveCount(0);
  await expect.poll(() => hits.length).toBe(1);
  const node = await bubble.elementHandle();
  const inputId = api.id();
  await advance(hub, id, hits[0]!.key, inputId, [{ state: 'dispatched', seq: '101' }, { state: 'on_machine', seq: '102' }]);
  await expect(bubble).toContainText('На машине');
  await expect(page.locator('.strip .item')).toHaveCount(0);
  hub.append(`conversation:${id}`, { kind: 'input_state_changed', conversation_id: id, payload: { input_id: inputId, state: 'transmitting' } });
  hub.flush();
  await expect(bubble).toContainText('Передаётся');
  await expect(page.locator('.bubble', { hasText: 'сразу в ленту' })).toHaveCount(1);
  expect(await node!.evaluate((el) => el.isConnected)).toBe(true);
  await expect(pill).toHaveCount(0);
  await expect(page.locator('.feed')).toHaveAttribute('data-pinned', '');
});

test('send_during_a_running_turn_keeps_the_chip_and_no_early_bubble', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  hub.append(`conversation:${id}`, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: {}, since: AT },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.flush();
  const hits = await watch(page, '**/api/v1/conversations/*/inputs');
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('ждёт ход');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => hits.length).toBe(1);
  await expect(page.locator('.strip .item', { hasText: 'ждёт ход' })).toBeVisible();
  await expect(page.locator('.bubble', { hasText: 'ждёт ход' })).toHaveCount(0);
  void api;
});

test('a_refused_send_removes_the_bubble_and_returns_the_text', async ({ page }) => {
  await boot(page);
  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    await route.fulfill({
      status: 422,
      headers: { 'X-Server-Time-Ms': String(Date.now()) },
      json: { error: { code: 'invalid', message: 'Отклонено хабом', scope: 'delivery', retry: 'never', correlation_id: '00000000-0000-4000-8000-0000000000e3' } }
    });
  });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill('не пройдёт');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect(area).toHaveValue('не пройдёт');
  await expect(page.locator('.bubble', { hasText: 'не пройдёт' })).toHaveCount(0);
});

async function sendWatchingMotion(page: Page) {
  await page.getByRole('textbox', { name: 'Сообщение' }).fill('с движением');
  await page.evaluate(() => {
    const seen = { count: 0 };
    (window as unknown as { __enter: typeof seen }).__enter = seen;
    new MutationObserver(() => {
      for (const el of document.querySelectorAll('.bubble')) seen.count = Math.max(seen.count, el.getAnimations().length);
    }).observe(document.body, { childList: true, subtree: true });
  });
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect(page.locator('.bubble', { hasText: 'с движением' })).toBeVisible();
  await page.waitForTimeout(400);
  return page.evaluate(() => (window as unknown as { __enter: { count: number } }).__enter.count);
}

test('the_bubble_enters_with_the_motion_tokens', async ({ page }) => {
  await boot(page);
  expect(await sendWatchingMotion(page)).toBeGreaterThan(0);
});

test('the_bubble_does_not_animate_under_reduced_motion', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await boot(page);
  expect(await sendWatchingMotion(page)).toBe(0);
});

test('keyboard_sized_viewport_keeps_the_attachment_text_and_caret_above_the_keyboard', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'экранная клавиатура телефона');
  await boot(page);
  await page.setInputFiles('input[type=file]', {
    name: 'Screenshot_20261105_161407_Chrome.jpg',
    mimeType: 'image/jpeg',
    buffer: Buffer.from('screenshot')
  });
  const tray = page.locator('[data-composer] .tray');
  await expect(tray.getByText('Загружено')).toBeVisible({ timeout: 30_000 });

  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.fill(
    Array.from(
      { length: 14 },
      (_, index) => `строка ${index + 1}: длинный текст, который должен остаться над клавиатурой`
    ).join('\n')
  );
  await area.focus();
  await area.press('End');

  const height = 520;
  await page.setViewportSize({ width: 412, height });
  await expect(page.locator('html')).toHaveAttribute('data-compact', '');
  await expect.poll(() => page.locator('[data-composer]').evaluate((node) => node.getBoundingClientRect().bottom)).toBeLessThanOrEqual(height + 1);

  const box = (await page.locator('[data-composer]').boundingBox())!;
  expect(box.y + box.height).toBeLessThanOrEqual(height + 1);
  const cap = await page.locator('[data-composer]').evaluate((node) => parseFloat(getComputedStyle(node).maxHeight));
  expect(box.height).toBeLessThanOrEqual(cap + 1);
  expect((await area.boundingBox())!.y + (await area.boundingBox())!.height).toBeLessThanOrEqual(height + 1);

  // Поле прокручивается внутри, поэтому последняя строка с курсором видна.
  await expect
    .poll(() =>
      area.evaluate((node) => node.scrollTop + node.clientHeight >= node.scrollHeight - 2)
    )
    .toBe(true);
  const field = await area.evaluate((node) => ({
    scrollTop: node.scrollTop,
    scrollHeight: node.scrollHeight,
    clientHeight: node.clientHeight
  }));
  expect(field.scrollTop).toBeGreaterThan(0);
  expect(field.scrollTop + field.clientHeight).toBeGreaterThanOrEqual(field.scrollHeight - 2);

  // Вложения в компактном виде едут одной строкой вбок.
  const trayLayout = await tray.evaluate((node) => ({
    height: node.getBoundingClientRect().height,
    direction: getComputedStyle(node.querySelector('ul')!).flexDirection,
    overflow: getComputedStyle(node.querySelector('ul')!).overflowX
  }));
  expect(trayLayout.height).toBeLessThanOrEqual(80);
  expect(trayLayout.direction).toBe('row');
  expect(trayLayout.overflow).toBe('auto');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});
