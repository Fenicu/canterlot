import { createHash } from 'node:crypto';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
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

function block(api: FakeHubApi, seq: string, partial: Partial<HistoryBlock>): HistoryBlock {
  const id = api.id();
  const content = partial.content ?? 'вывод';
  const at = new Date(Date.now() - 60_000).toISOString();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return { kind: 'block', id, block_kind: 'text', content, content_bytes: content.length, content_truncated: false, created_seq: seq, created_at: at, rev: 1, state: 'final', agent: 'claude', model: 'opus-5.5', tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1, started_at: at, finished_at: new Date(Date.now() - 59_000).toISOString(), ...partial };
}

function startTurn(hub: FakeHub, api: FakeHubApi, id: string): void {
  const scope = `conversation:${id}`;
  hub.append(scope, { kind: 'derived_changed', conversation_id: id, payload: { conversation_id: id, activity: { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() }, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: new Date().toISOString(), settings: { model: 'opus-5.5', effort: 'high', permission_mode: 'owner' }, crunch: { enabled: false } } });
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

  test('send_stays_on_one_row_with_attach_when_a_turn_starts', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    await page.getByRole('textbox', { name: 'Сообщение' }).fill('уточнение');
    const send = page.getByRole('button', { name: 'Отправить', exact: true });
    const before = await send.boundingBox();
    startTurn(hub, api, id);
    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await expect(page.getByRole('menuitem', { name: 'Прервать ход' })).toBeVisible();
    await page.keyboard.press('Escape');
    // Прервать ход можно в ленте и в меню сессии, но не рядом с «Отправить».
    await expect(page.locator('[data-composer]').getByRole('button', { name: /Прерв/ })).toHaveCount(0);
    const after = await send.boundingBox();
    const attach = await page.getByRole('button', { name: 'Прикрепить файл' }).boundingBox();
    expect(after!.x).toBeCloseTo(before!.x, 0);
    expect(after!.y).toBeCloseTo(before!.y, 0);
    expect(attach!.y).toBeCloseTo(after!.y, 0);
  });

  test('field_grows_up_to_six_lines_and_the_capsule_carries_the_focus_ring', async ({ page }) => {
    await boot(page);
    const field = page.getByRole('textbox', { name: 'Сообщение' });
    const line = await field.evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));
    const empty = (await field.boundingBox())!.height;
    await field.fill(['раз', 'два', 'три'].join('\n'));
    await expect.poll(async () => (await field.boundingBox())!.height).toBeGreaterThan(empty + line);
    await field.fill(Array.from({ length: 14 }, (_, index) => `строка ${index}`).join('\n'));
    await expect.poll(async () => (await field.boundingBox())!.height).toBeGreaterThan(line * 5);
    const capped = (await field.boundingBox())!.height;
    expect(capped).toBeLessThan(line * 7);
    await field.focus();
    const ring = await page.locator('.capsule').evaluate((node) => getComputedStyle(node).outlineStyle);
    expect(ring).toBe('solid');
    expect(await field.evaluate((node) => getComputedStyle(node).outlineStyle)).toBe('none');
  });
});

test.describe('строки инструментов и сессий', () => {
  test('tool_row_keeps_the_name_whole_and_the_status_at_the_right_edge', async ({ page }) => {
    const { api, id } = await boot(page);
    api.histories.set(id, [block(api, '1', { block_kind: 'tool_call', tool_name: 'Bash', summary: 'cargo test -p canterlot-hub --test stream -- --nocapture reconnect_after_sleep', content: 'ok' })]);
    api.bindConversation(id);
    await page.reload();
    const head = page.locator('.tool .head');
    await expect(head).toBeVisible();
    const box = await head.evaluate((node) => {
      const name = node.querySelector('.name')!;
      const summary = node.querySelector('.summary')!;
      const pill = node.querySelector('.pill')!;
      const style = getComputedStyle(name);
      return { nameHeight: name.getBoundingClientRect().height, line: parseFloat(style.lineHeight) || parseFloat(style.fontSize) * 1.6, summaryLines: summary.getBoundingClientRect().height, gap: node.getBoundingClientRect().right - pill.getBoundingClientRect().right, ellipsis: getComputedStyle(summary).textOverflow, chevron: node.querySelectorAll('svg').length };
    });
    expect(box.nameHeight).toBeLessThan(box.line * 1.5);
    expect(box.summaryLines).toBeLessThan(box.line * 1.5);
    expect(box.gap).toBeLessThan(24);
    expect(box.ellipsis).toBe('ellipsis');
    expect(box.chevron).toBe(1);
  });

  test('session_row_shows_unread_or_time_and_never_lays_them_over_the_meta_line', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedMockupSessions(api);
    for (const row of api.list.values()) row.last_activity_at = new Date().toISOString();
    await installChatRoutes(page, hub, api, ids.pinned);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    const rows = await page.locator('[data-session]').evaluateAll((nodes) => nodes.map((node) => {
      const time = node.querySelector('time')?.getBoundingClientRect();
      const meta = node.querySelector('.meta')!.getBoundingClientRect();
      const title = node.querySelector('.title')!.getBoundingClientRect();
      return { time: time ? { top: time.top, bottom: time.bottom, left: time.left } : null, metaTop: meta.top, titleRight: title.right, unread: node.querySelector('.unread, .count') !== null };
    }));
    expect(rows.length).toBeGreaterThan(3);
    for (const row of rows) {
      if (row.unread) expect(row.time).toBeNull();
      if (row.time) {
        expect(row.time.bottom).toBeLessThanOrEqual(row.metaTop + 1);
        expect(row.time.left).toBeGreaterThanOrEqual(row.titleRight - 1);
      }
    }
  });
});

test.describe('шапка и сообщения', () => {
  test('phone_header_has_one_back_control_and_a_full_width_title_row', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, id } = await boot(page);
    await expect(page.getByRole('link', { name: 'Назад' })).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Все сессии' })).toHaveCount(1);
    hub.append(`conversation:${id}`, { kind: 'derived_changed', conversation_id: id, payload: { conversation_id: id, activity: { state: 'waiting_for_user', evidence: { question: 'Что делать, если курсор старше журнала событий и повтор уже невозможен?' }, since: new Date().toISOString() }, unread_count: 0, queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, last_activity_at: new Date().toISOString(), settings: { model: 'opus-5.5', effort: 'high', permission_mode: 'owner' }, crunch: { enabled: false } } });
    hub.flush();
    await expect(page.locator('.state-chip .state-text')).toHaveText('Ждёт ответа');
    const sizes = await page.locator('.chat-header').evaluate((node) => ({
      title: node.querySelector('.header-title')!.getBoundingClientRect().width,
      chip: node.querySelector('.state-chip')!.getBoundingClientRect().height,
      bar: node.querySelector('.topbar')!.getBoundingClientRect().height
    }));
    expect(sizes.title).toBeGreaterThan(190);
    expect(sizes.chip).toBeLessThan(24);
    expect(sizes.bar).toBeLessThan(70);
    await expect(page.locator('.chat-header')).not.toContainText('курсор старше');
  });

  test('bubbles_keep_a_gutter_sit_right_and_only_the_last_input_shows_the_working_track', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    const input = (text: string, seq: string) => ({ kind: 'input', id: api.id(), idempotency_key: `k-${seq}`, state: 'accepted', content: [{ type: 'text', text }], attachments: [], created_seq: seq, feed_seq: seq, created_at: new Date().toISOString() });
    api.histories.set(id, [input('Первое', '1'), block(api, '2', { content: 'Ответ' }), input('Второе', '3'), block(api, '4', { content: 'Идёт', state: 'streaming' })] as never);
    api.bindConversation(id);
    await page.reload();
    await expect(page.locator('article.bubble')).toHaveCount(2);
    const boxes = await page.locator('article.bubble').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect()).map(({ x, right }) => ({ x, right })));
    for (const box of boxes) {
      expect(box.x).toBeGreaterThan(8);
      expect(box.right).toBeLessThanOrEqual(390 - 8);
    }
    await expect(page.locator('article.bubble').first()).toContainText('Принято агентом');
    await expect(page.locator('article.bubble').first()).not.toContainText('Агент работает');
    await expect(page.locator('article.bubble').nth(1).locator('.st-status')).toHaveText('Агент работает');
    await expect(page.locator('[data-row]').nth(2)).toBeVisible();
    await expect(page.getByText('Принято агентом', { exact: false })).toHaveCount(1);
    void hub;
  });

  test('new_messages_pill_sits_at_the_bottom_centre_of_the_feed', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    api.histories.set(id, Array.from({ length: 30 }, (_, index) => block(api, String(index - 30), { content: `Ответ ${index}\n\nЕщё абзац текста, чтобы лента прокручивалась.` })));
    api.bindConversation(id);
    await page.reload();
    await expect(page.locator('[data-row]').first()).toBeVisible();
    const feed = page.locator('.feed');
    await expect.poll(() => feed.evaluate((node) => node.scrollHeight - node.clientHeight)).toBeGreaterThan(500);
    await feed.evaluate((node) => { node.scrollTop -= 500; });
    await page.waitForTimeout(200);
    hub.append(`conversation:${id}`, { kind: 'block_started', conversation_id: id, payload: { block_id: api.id(), rev: 0, kind: 'text', input_command_id: null, parent_block_id: null, agent: 'claude', model: 'opus-5.5', tool_name: null, summary: null, approved_by: null } });
    hub.flush();
    const pill = page.getByRole('button', { name: 'К новым сообщениям' });
    await expect(pill).toBeVisible();
    const box = (await pill.boundingBox())!;
    const wrap = (await page.locator('.feed-wrap').boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(44);
    expect(Math.abs(box.x + box.width / 2 - (wrap.x + wrap.width / 2))).toBeLessThan(2);
    expect(wrap.y + wrap.height - (box.y + box.height)).toBeLessThan(24);
    const paint = await pill.evaluate((node) => {
      const probe = document.createElement('i');
      node.append(probe);
      const read = (token: string): string => {
        probe.style.color = `var(${token})`;
        return getComputedStyle(probe).color;
      };
      const pair = { accent: read('--accent'), ink: read('--accent-ink') };
      probe.remove();
      const style = getComputedStyle(node);
      return { ...pair, fill: style.backgroundColor, text: style.color };
    });
    expect(paint.fill).toBe(paint.accent);
    expect(paint.text).toBe(paint.ink);
  });
});

const MARKDOWN = `## Причина

Первый абзац со [ссылкой](https://example.org/doc) и кодом \`resync_required\`.

> Цитата из журнала.

- пункт раз
- пункт два

| Сценарий | Было |
| --- | --- |
| Сон 30 с | теряет дельты |

\`\`\`rust
fn main() { println!("привет"); }
\`\`\`

Слово ${'Превысокомногорассмотрительствующий'.repeat(4)} в конце.`;

test.describe('текст ответа', () => {
  test('prose_elements_get_their_styles_and_long_words_stay_inside_the_card', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { api, id } = await boot(page);
    api.histories.set(id, [block(api, '1', { content: MARKDOWN })]);
    api.bindConversation(id);
    await page.reload();
    const text = page.locator('.text');
    await expect(text.locator('a')).toBeVisible();
    const style = await text.evaluate((node) => {
      const css = (selector: string) => getComputedStyle(node.querySelector(selector)!);
      const body = getComputedStyle(node);
      return {
        link: [css('a').textDecorationLine, css('a').color !== body.color],
        pre: [css('pre').backgroundColor, css('pre').paddingTop],
        inline: css('p code').backgroundColor,
        quote: [css('blockquote').backgroundColor, css('blockquote').borderLeftWidth, css('blockquote').marginLeft],
        th: css('th').textAlign,
        heading: css('h2').marginTop,
        overflow: node.scrollWidth - node.clientWidth
      };
    });
    expect(style.link).toEqual(['underline', true]);
    expect(style.pre[0]).not.toBe('rgba(0, 0, 0, 0)');
    expect(parseFloat(style.pre[1]!)).toBeGreaterThan(0);
    expect(style.inline).not.toBe('rgba(0, 0, 0, 0)');
    expect(style.quote[0]).not.toBe('rgba(0, 0, 0, 0)');
    expect(style.quote.slice(1)).toEqual(['0px', '0px']);
    expect(style.th).toBe('left');
    expect(style.overflow).toBeLessThanOrEqual(0);
  });

  test('code_is_highlighted_only_after_the_block_is_final', async ({ page }) => {
    const { hub, api, id } = await boot(page);
    const streaming = block(api, '1', { content: '```rust\nfn main() {}\n```\n', state: 'streaming' });
    api.blocks.set(streaming.id, { text: streaming.content, rev: 1, state: 'streaming' });
    api.histories.set(id, [streaming]);
    api.bindConversation(id);
    await page.reload();
    await expect(page.locator('.text pre')).toBeVisible();
    await page.waitForTimeout(300);
    await expect(page.locator('.text .hljs-keyword')).toHaveCount(0);
    hub.append(`conversation:${id}`, { kind: 'block_final', conversation_id: id, payload: { block_id: streaming.id, rev: 2, text: streaming.content, checksum: createHash('sha256').update(streaming.content).digest('hex'), text_omitted: false, summary: null } });
    hub.flush();
    await expect(page.locator('.text .hljs-keyword').first()).toBeVisible();
  });

  test('wide_code_may_leave_the_column_on_a_large_screen_but_stays_inside_the_pane', async ({ page }) => {
    await page.setViewportSize({ width: 2560, height: 1440 });
    const { api, id } = await boot(page);
    api.histories.set(id, [block(api, '1', { content: `\`\`\`text\n${'длинная строка вывода '.repeat(20)}\n\`\`\`` })]);
    api.bindConversation(id);
    await page.reload();
    const pre = page.locator('.text pre');
    await expect(pre).toBeVisible();
    const box = (await pre.boundingBox())!;
    const card = (await page.locator('article.card').boundingBox())!;
    const pane = (await page.locator('.feed-wrap').boundingBox())!;
    expect(box.width).toBeGreaterThan(card.width);
    expect(box.x + box.width).toBeLessThanOrEqual(pane.x + pane.width);
  });
});

test.describe('листы и панели', () => {
  async function openMenu(page: Page, item: string) {
    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await page.getByRole('menuitem', { name: item }).click();
  }

  test('settings_and_diagnostics_show_a_single_title_and_a_single_close', async ({ page, isMobile }) => {
    await boot(page);
    await openMenu(page, 'Диагностика');
    const dialog = page.getByRole('dialog', { name: 'Диагностика' });
    await expect(dialog.getByRole('heading', { name: 'Диагностика' })).toHaveCount(1);
    await expect(dialog.getByRole('button', { name: 'Закрыть' })).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await openMenu(page, 'Настройки сессии');
    const scope = isMobile ? page.getByRole('dialog', { name: 'Настройки сессии' }) : page.locator('[data-pane="lane"]');
    await expect(scope.getByRole('heading', { name: 'Настройки сессии' })).toHaveCount(1);
    await expect(scope.getByRole('button', { name: 'Закрыть' })).toHaveCount(1);
  });

  test('settings_sheet_keeps_its_title_in_view_and_the_save_area_blends_into_the_surface', async ({ page, isMobile }) => {
    test.skip(!isMobile, 'только телефон');
    await boot(page);
    await openMenu(page, 'Настройки сессии');
    const dialog = page.getByRole('dialog', { name: 'Настройки сессии' });
    const body = dialog.locator('.body');
    const title = dialog.getByRole('heading', { name: 'Настройки сессии' });
    const footer = dialog.locator('.footer-action');
    // Лист въезжает снизу: заголовок сравнивается только после конца анимации.
    await dialog.evaluate((node) => Promise.all(node.getAnimations().map((animation) => animation.finished)));
    const top = (await title.boundingBox())!.y;
    await body.evaluate((node) => { node.scrollTop = node.scrollHeight; });
    expect((await title.boundingBox())!.y).toBe(top);
    const paint = await footer.evaluate((node) => {
      const sheet = getComputedStyle(node.closest('dialog')!).backgroundColor;
      return { same: getComputedStyle(node).backgroundColor === sheet, fade: getComputedStyle(node, '::before').backgroundImage };
    });
    expect(paint.same).toBe(true);
    expect(paint.fade).toContain('linear-gradient');
    const last = (await dialog.locator('#resume').boundingBox())!;
    const save = (await footer.getByRole('button', { name: /Сохранить настройки/ }).boundingBox())!;
    expect(last.y + last.height).toBeLessThanOrEqual(save.y + 1);
    const sheet = (await dialog.boundingBox())!;
    const foot = (await footer.boundingBox())!;
    expect(Math.abs(foot.y + foot.height - (sheet.y + sheet.height))).toBeLessThanOrEqual(1);
  });

  test('desktop_keeps_the_chat_visible_settings_open_in_the_pane_and_sheets_have_no_handle', async ({ page, isMobile }) => {
    test.skip(isMobile, 'только рабочий стол');
    await boot(page);
    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
    await expect(page.locator('[data-pane="lane"] #model')).toBeVisible();
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeEditable();
    await openMenu(page, 'Диагностика');
    const dialog = page.getByRole('dialog', { name: 'Диагностика' });
    await expect(dialog.locator('.handle')).toBeHidden();
    const backdrop = await dialog.evaluate((node) => getComputedStyle(node, '::backdrop').backgroundColor);
    expect(backdrop).not.toMatch(/^rgb\(/);
  });

  test('delegation_action_lives_in_the_header_and_only_appears_with_delegations', async ({ page, isMobile }) => {
    test.skip(isMobile, 'только рабочий стол');
    const { api, id } = await boot(page);
    await expect(page.getByRole('button', { name: 'Делегирование' })).toHaveCount(0);
    api.histories.set(id, [block(api, '1', { block_kind: 'tool_call', tool_name: 'Agent', summary: 'поручение' })]);
    api.bindConversation(id);
    await page.reload();
    const action = page.locator('.topbar').getByRole('button', { name: 'Делегирование' });
    await expect(action).toBeVisible();
    await action.click();
    await expect(page.locator('[data-pane="lane"]')).toBeVisible();
    await expect(action).toHaveAttribute('aria-pressed', 'true');
    const edges = await page.evaluate(() => ['.topbar', 'article.card', '.capsule'].map((selector) => document.querySelector(selector)?.getBoundingClientRect().left));
    expect(Math.max(...(edges as number[])) - Math.min(...(edges as number[]))).toBeLessThan(1);
  });

  test('session_popover_closes_on_escape_and_sits_beside_its_row', async ({ page, isMobile }) => {
    test.skip(isMobile, 'только рабочий стол');
    await page.setViewportSize({ width: 1440, height: 1300 });
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedMockupSessions(api);
    await installChatRoutes(page, hub, api, ids.pinned);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    const row = page.locator(`[data-session="${ids.pinned}"]`);
    await row.hover();
    await row.getByRole('button', { name: /^Действия/ }).click();
    const popover = page.getByRole('dialog', { name: 'Сессия' });
    const list = await page.locator('[data-list-scroll]').boundingBox();
    const box = (await popover.boundingBox())!;
    const rowBox = (await row.boundingBox())!;
    expect(box.y >= rowBox.y + rowBox.height - 1 || box.y + box.height <= rowBox.y + 1).toBe(true);
    expect(box.y + box.height).toBeLessThanOrEqual(list!.y + list!.height + 80);
    await page.keyboard.press('Escape');
    await expect(popover).toHaveCount(0);
  });
});

test.describe('очередь и вложения', () => {
  async function queue(hub: FakeHub, api: FakeHubApi, id: string, texts: string[]) {
    texts.forEach((text, index) => hub.append(`conversation:${id}`, { kind: 'input_queued', conversation_id: id, payload: { input_id: api.id(), input_seq: String(index + 20), idempotency_key: api.id(), state: 'queued', content: [{ type: 'text', text }] } }));
    hub.flush();
  }

  test('queue_pills_are_one_line_stay_in_the_gutter_and_collapse_above_three', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const { hub, api, id } = await boot(page);
    await queue(hub, api, id, ['После тестов обнови README и добавь строку про порядок подписки и снимка журнала', 'Потом собери релиз']);
    const strip = page.locator('.strip');
    await expect(strip.locator('[data-state]')).toHaveCount(2);
    const first = await strip.locator('.pill').first().boundingBox();
    expect(first!.height).toBeLessThanOrEqual(48);
    expect(first!.x).toBeGreaterThan(8);
    // Действия правки живут внутри строки, не добавляя второй строки к каждой заявке.
    await expect(strip.locator('.pill').first().getByRole('button', { name: 'Изменить' })).toBeVisible();
    // Обрезанный текст читается в подсказке, а кнопки называют своё сообщение.
    await expect(strip.locator('.pill .text').first()).toHaveAttribute('title', 'После тестов обнови README и добавь строку про порядок подписки и снимка журнала');
    await expect(strip.locator('.pill').nth(1).getByRole('button', { name: 'Отменить: Потом собери релиз' })).toBeVisible();
    expect((await strip.boundingBox())!.height).toBeLessThan(110);
    await queue(hub, api, id, ['Третье', 'Четвёртое']);
    await expect(strip.getByRole('button', { name: 'Ещё 2 в очереди' })).toBeVisible();
    await expect(strip.locator('[data-state]')).toHaveCount(2);
    await strip.getByRole('button', { name: 'Ещё 2 в очереди' }).click();
    await expect(strip.locator('[data-state]')).toHaveCount(4);
  });

  test('sent_attachments_stay_visible_in_the_bubble', async ({ page }) => {
    const { api, id } = await boot(page);
    api.histories.set(id, [{ kind: 'input', id: api.id(), idempotency_key: 'k-1', state: 'accepted', content: [{ type: 'text', text: 'Лог приложен' }], attachments: [{ attachment_id: api.id(), name: 'hub.log', size: 2048, mime: 'text/plain', sha256: 'x', image: false }], created_seq: '1', feed_seq: '1', created_at: new Date().toISOString() }] as never);
    api.bindConversation(id);
    await page.reload();
    await expect(page.locator('article.bubble')).toContainText('hub.log');
  });

  test('image_tile_opens_the_viewer_and_a_pending_image_has_its_own_tile', async ({ page }) => {
    const { api, id } = await boot(page);
    api.histories.set(id, [block(api, '1', { block_kind: 'tool_call', tool_name: 'Screenshot', summary: 'экран чата', media: [{ media_id: api.id(), mime: 'image/png', width: 320, height: 180, ready: true }, { media_id: api.id(), mime: 'image/png', width: 320, height: 180, ready: false }] })]);
    api.bindConversation(id);
    await page.reload();
    await expect(page.getByText('Картинка загружается')).toBeVisible();
    await page.getByRole('button', { name: 'Открыть картинку' }).click();
    const dialog = page.getByRole('dialog', { name: 'Картинки ответа' });
    await expect(dialog.locator('img')).toBeVisible();
    await expect(dialog.getByRole('link', { name: 'Сохранить' })).toBeVisible();
  });
});

test.describe('лимиты и баннеры', () => {
  async function list(page: Page) {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedMockupSessions(api);
    await installChatRoutes(page, hub, api, ids.pinned);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    return { hub, api, ids };
  }

  test('limits_strip_colours_only_the_low_value_and_keeps_providers_apart', async ({ page }) => {
    await list(page);
    const strip = page.locator('.limits').first();
    await expect(strip.locator('.row')).toHaveCount(2);
    await expect(strip.locator('.pct[data-tone="danger"]')).toHaveText([/4%/]);
    await expect(strip.locator('.pct[data-tone="warning"]')).toHaveCount(0);
    await expect(strip.locator('.meter')).toHaveCount(4);
    await strip.locator('summary').click();
    const radius = await strip.evaluate((node) => parseFloat(getComputedStyle(node).borderTopLeftRadius));
    expect(radius).toBeLessThan(40);
  });

  test('archive_tab_explains_that_it_is_empty', async ({ page }) => {
    await list(page);
    await page.getByRole('tab', { name: /Архив/ }).click();
    await expect(page.getByText('В архиве пока пусто')).toBeVisible();
  });

});
