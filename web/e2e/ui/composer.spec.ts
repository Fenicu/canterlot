import { mkdtempSync, truncateSync, writeFileSync } from 'node:fs';
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
      settings: { model: 'opus', effort: 'medium', permission_mode: 'default' },
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
      settings: { model: 'opus', effort: 'medium', permission_mode: 'default' },
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
  await expect(page.getByText('в очереди')).toBeVisible();
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
  const over = join(dir, 'over.bin');
  writeFileSync(over, '');
  truncateSync(over, 104_857_601);
  await page.setInputFiles('input[type=file]', over);
  await expect(page.getByText('Файл больше 100 МиБ')).toBeVisible();
  await expect(page.getByText('over.bin')).toHaveCount(0);
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
