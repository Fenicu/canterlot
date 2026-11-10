import type { Page } from '@playwright/test';
import { sha256Hex } from '../../src/lib/stores/blocks';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const HISTORY_AT = '2026-10-28T12:00:00.000Z';

type Boot = { hub: FakeHub; api: FakeHubApi; id: string };

function scope(id: string): string {
  return `conversation:${id}`;
}

const base = (api: FakeHubApi) => ({
  input_command_id: api.id(),
  parent_block_id: null,
  agent: 'claude',
  model: 'opus-5.5',
  tool_name: null,
  summary: null,
  approved_by: null
});

/** История с готовыми блоками: рассуждение в ней должно ждать владельца, а не открываться само. */
function seedBlocks(api: FakeHubApi, id: string, items: { kind: 'text' | 'reasoning'; content: string }[]): void {
  const blocks: HistoryBlock[] = items.map((item, index) => {
    const blockId = api.id();
    api.blocks.set(blockId, { text: item.content, rev: 1, state: 'final' });
    return {
      kind: 'block', id: blockId, block_kind: item.kind, content: item.content, content_bytes: item.content.length, content_truncated: false,
      // Номера истории стоят до первого живого события: у него свой счёт журнала.
      created_seq: String(index - items.length), created_at: new Date(Date.parse(HISTORY_AT) + index * 1000).toISOString(), rev: 1, state: 'final',
      agent: 'claude', model: 'opus-5.5', tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1
    };
  });
  api.histories.set(id, blocks);
  api.bindConversation(id);
}

async function boot(page: Page, opts: { history?: number; blocks?: { kind: 'text' | 'reasoning'; content: string }[]; pref?: 'line' | 'live' | 'expanded' } = {}): Promise<Boot> {
  await page.addInitScript((pref) => {
    if (pref) localStorage.setItem('canterlot.reasoning', pref);
    else localStorage.removeItem('canterlot.reasoning');
  }, opts.pref ?? null);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'рассуждение', model: 'opus-5.5' });
  if (opts.history || opts.blocks) {
    const items = opts.blocks ?? Array.from({ length: opts.history! }, (_, index) => ({ kind: 'text' as const, content: `строка истории ${index + 1}` }));
    seedBlocks(api, id, items);
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  if (opts.history || opts.blocks) await expect(page.locator('[data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function startReasoning(hub: FakeHub, api: FakeHubApi, id: string, at?: string): { blockId: string; run: string } {
  const blockId = api.id();
  const run = api.id();
  hub.append(scope(id), {
    kind: 'block_started',
    conversation_id: id,
    payload: { ...base(api), input_command_id: run, block_id: blockId, rev: 0, kind: 'reasoning' },
    ...(at ? { at } : {})
  });
  return { blockId, run };
}

function delta(hub: FakeHub, id: string, blockId: string, rev: number, append: string): void {
  hub.append(scope(id), { kind: 'block_delta', conversation_id: id, payload: { block_id: blockId, base_rev: rev - 1, rev, append } });
}

async function finalize(hub: FakeHub, id: string, blockId: string, rev: number, text: string): Promise<void> {
  hub.append(scope(id), {
    kind: 'block_final', conversation_id: id,
    payload: { block_id: blockId, rev, text, checksum: await sha256Hex(text), text_omitted: false, summary: null }
  });
  hub.flush();
}

/** Прокрутка рукой владельца: колесо приходит раньше сдвига, иначе прижатая лента вернётся к низу. */
async function scrollUp(page: Page, pixels: number): Promise<void> {
  await page.locator('.feed').evaluate((el, by) => {
    el.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    el.scrollBy(0, -by);
  }, pixels);
}

const feedGap = (page: Page) => page.locator('.feed').evaluate((el) => el.scrollHeight - el.clientHeight - el.scrollTop);
const noOverflow = (page: Page) => page.evaluate(() => [...document.querySelectorAll<HTMLElement>('.feed, .card, .reasoning')]
  .every((el) => el.scrollWidth <= el.clientWidth + 1));

test('the_live_window_keeps_its_height_and_folds_into_a_line_with_a_preview', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const { blockId } = startReasoning(hub, api, id, new Date(Date.now() - 12_000).toISOString());
  const chunks = ['Проверяю модуль.', '\nСверяю результат.', '\nСмотрю тесты.', '\nСобираю вывод.'];
  const row = page.locator('.reasoning');
  let height = 0;
  for (let index = 0; index < chunks.length; index++) {
    delta(hub, id, blockId, index + 1, chunks[index]!);
    hub.flush();
    const window = row.locator('.window');
    await expect(window).toBeVisible();
    await expect(window).toContainText(chunks[index]!.trim());
    const box = (await window.boundingBox())!;
    if (index === 0) height = box.height;
    else expect(Math.abs(box.height - height), 'window height across deltas').toBeLessThanOrEqual(0.5);
    await expect(row.locator('.toggle')).toContainText('Рассуждает');
    // Окно само говорит о рассуждении: строка хода не повторяет эти слова, но остаётся с прерыванием.
    const line = page.locator('[data-working]');
    await expect(line).toBeVisible();
    await expect(line).toHaveAttribute('data-quiet', '');
    await expect(line.locator('.text')).toHaveCSS('position', 'absolute');
    await expect(line.getByRole('button', { name: 'Прервать' })).toBeVisible();
  }
  await finalize(hub, id, blockId, chunks.length, chunks.join(''));
  await expect(row.locator('.window')).toHaveCount(0, { timeout: 5_000 });
  await expect(row.locator('.preview')).toHaveText('Проверяю модуль.');
  await expect(row.locator('.duration')).toHaveText(/^\d+ с$/);
  await expect(row.locator('.toggle')).toHaveAttribute('aria-expanded', 'false');
  await expect(page.locator('[data-working]')).toHaveCount(0);
});

test('the_reasoning_preference_is_chosen_in_settings_and_kept_after_reload', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 700 });
  await bootSecondary(page, '/settings');
  const group = page.locator('main').getByRole('radiogroup', { name: 'Рассуждения' });
  await expect(group).toBeVisible();
  await expect(group.getByRole('radio', { name: 'Вживую', exact: true })).toBeChecked();
  const stored = () => page.evaluate(() => localStorage.getItem('canterlot.reasoning'));
  // Загрузка настроек ничего не пишет: выбор сохраняется только действием владельца.
  await expect.poll(stored).toBeNull();
  // Три варианта помещаются в узкий экран, цель нажатия не меньше токена.
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1), 'no horizontal scroll').toBe(true);
  for (const label of ['Строкой', 'Вживую', 'Раскрыты']) {
    const box = (await group.locator('label').filter({ hasText: label }).locator('span').boundingBox())!;
    expect(box.height, `tap target ${label}`).toBeGreaterThanOrEqual(44);
  }
  await group.getByRole('radio', { name: 'Строкой', exact: true }).check();
  await expect.poll(stored).toBe('line');
  await group.getByRole('radio', { name: 'Раскрыты', exact: true }).check();
  await expect.poll(stored).toBe('expanded');
  await page.reload();
  await expect(page.locator('main').getByRole('radiogroup', { name: 'Рассуждения' }).getByRole('radio', { name: 'Раскрыты', exact: true })).toBeChecked();
});

test('the_reasoning_preference_chosen_in_settings_sets_the_presentation_in_the_chat', async ({ page }) => {
  const { api, ids } = await bootSecondary(page, '/settings');
  const text = '## План\n- **Проверить** модуль';
  seedBlocks(api, ids.pinned, [{ kind: 'reasoning', content: text }]);
  const group = page.locator('main').getByRole('radiogroup', { name: 'Рассуждения' });
  await group.getByRole('radio', { name: 'Строкой', exact: true }).check();
  await page.goto(`/c/${ids.pinned}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const reasoning = page.locator('.reasoning');
  await expect(reasoning).toBeVisible();
  await expect(reasoning.locator('.fold')).toHaveCount(0);
  await expect(reasoning.locator('.preview')).toHaveText('План');
  await expect(reasoning.locator('.toggle')).toHaveAttribute('aria-expanded', 'false');

  await page.goto('/settings');
  const again = page.locator('main').getByRole('radiogroup', { name: 'Рассуждения' });
  await expect(again).toBeVisible();
  await again.getByRole('radio', { name: 'Раскрыты', exact: true }).check();
  await page.goto(`/c/${ids.pinned}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.reasoning .fold')).toBeVisible();
  await expect(page.locator('.reasoning .fold')).toContainText('Проверить');
  await expect(page.locator('.reasoning .toggle')).toHaveAttribute('aria-expanded', 'true');
});

for (const pref of ['line', 'live', 'expanded'] as const) {
  test(`the_interrupt_stays_reachable_while_the_agent_reasons_${pref}`, async ({ page }) => {
    const { hub, api, id } = await boot(page, { pref });
    const { blockId, run } = startReasoning(hub, api, id);
    delta(hub, id, blockId, 1, 'Думаю над задачей');
    hub.flush();
    const line = page.locator('[data-working]');
    const stop = line.locator('[data-interrupt]');
    await expect(line).toBeVisible();
    await expect(stop).toBeVisible();
    await expect(stop).toBeEnabled();
    // Фаза остаётся в доступном имени, хотя текст не повторяет окно рассуждения.
    await expect(line).toHaveAttribute('data-quiet', '');
    await expect(line).toContainText(/Рассуждает · \d+ с/);
    const thinking = (await stop.boundingBox())!;

    // Следующая фаза: активным становится вызов инструмента, строка говорит уже о нём.
    const tool = api.id();
    hub.append(scope(id), {
      kind: 'block_started', conversation_id: id,
      payload: { ...base(api), input_command_id: run, block_id: tool, rev: 0, kind: 'tool_call', tool_name: 'Bash', summary: 'ls' }
    });
    delta(hub, id, tool, 1, '{"command":"ls"}');
    hub.flush();
    await expect(line).not.toHaveAttribute('data-quiet', '');
    await expect(line).toContainText('Выполняет');
    const working = (await stop.boundingBox())!;
    // Кнопка не прыгает между фазами: её место у правого края одно и то же.
    expect(Math.abs(working.x - thinking.x), 'interrupt stays in place').toBeLessThanOrEqual(1);
    expect(working.height).toBeGreaterThanOrEqual(44);

    const sent = page.waitForRequest((request) => request.method() === 'POST' && request.url().endsWith('/interrupt'));
    await stop.click();
    await sent;
    await expect(line.getByRole('button', { name: 'Прерываю…' })).toBeVisible();
  });
}

test('a_tap_during_streaming_expands_the_block_and_it_stays_expanded_after_the_final', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const { blockId } = startReasoning(hub, api, id);
  delta(hub, id, blockId, 1, 'Сначала думаю');
  hub.flush();
  const row = page.locator('.reasoning');
  await expect(row.locator('.window')).toBeVisible();
  await row.locator('.toggle').click();
  await expect(row.locator('.window')).toHaveCount(0);
  await expect(row.locator('.fold')).toContainText('Сначала думаю');
  await expect(row.locator('.toggle')).toHaveAttribute('aria-expanded', 'true');
  delta(hub, id, blockId, 2, '\nи думаю дальше');
  hub.flush();
  await expect(row.locator('.fold')).toContainText('и думаю дальше');
  await finalize(hub, id, blockId, 2, 'Сначала думаю\nи думаю дальше');
  // Пауза автосворачивания не трогает выбор владельца.
  await page.waitForTimeout(1_200);
  await expect(row.locator('.fold')).toBeVisible();
  await expect(row.locator('.window')).toHaveCount(0);
  await expect(row.locator('.toggle')).toHaveAttribute('aria-expanded', 'true');
});

test('the_line_preference_never_opens_a_streaming_block_by_itself', async ({ page }) => {
  const { hub, api, id } = await boot(page, { pref: 'line' });
  const { blockId } = startReasoning(hub, api, id);
  delta(hub, id, blockId, 1, 'Первая мысль\nВторая мысль');
  hub.flush();
  const row = page.locator('.reasoning');
  await expect(row).toBeVisible();
  await expect(row.locator('.window')).toHaveCount(0);
  await expect(row.locator('.toggle')).toContainText('Рассуждает');
  await expect(row.locator('.preview')).toHaveText('Вторая мысль');
  await finalize(hub, id, blockId, 1, 'Первая мысль\nВторая мысль');
  await page.waitForTimeout(900);
  await expect(row.locator('.window')).toHaveCount(0);
  await expect(row.locator('.preview')).toHaveText('Первая мысль');
  await expect(row.locator('.toggle')).toHaveAttribute('aria-expanded', 'false');
});

test('the_expanded_preference_opens_finished_and_streaming_blocks', async ({ page }) => {
  const { hub, api, id } = await boot(page, {
    pref: 'expanded',
    blocks: [{ kind: 'text', content: 'ответ перед рассуждением' }, { kind: 'reasoning', content: 'готовое рассуждение истории' }]
  });
  // Загруженная история раскрыта сразу, без тапа.
  const history = page.locator('.reasoning').first();
  await expect(history.locator('.fold')).toBeVisible();
  await expect(history.locator('.toggle')).toHaveAttribute('aria-expanded', 'true');
  await expect(history.locator('.fold')).toContainText('готовое рассуждение истории');
  const { blockId } = startReasoning(hub, api, id);
  delta(hub, id, blockId, 1, 'Живая мысль');
  hub.flush();
  const live = page.locator('.reasoning').last();
  await expect(live.locator('.fold')).toContainText('Живая мысль');
  await expect(live.locator('.window')).toHaveCount(0);
  await expect(live.locator('.toggle')).toHaveAttribute('aria-expanded', 'true');
});

test('the_feed_stays_pinned_through_open_stream_and_fold', async ({ page }) => {
  const { hub, api, id } = await boot(page, { history: 30 });
  const feed = page.locator('.feed');
  await expect(feed).toHaveAttribute('data-pinned', '');
  const { blockId } = startReasoning(hub, api, id);
  for (let rev = 1; rev <= 4; rev++) {
    delta(hub, id, blockId, rev, `\nмысль ${rev}`);
    hub.flush();
    await expect.poll(() => feedGap(page)).toBeLessThanOrEqual(2);
    await expect(feed).toHaveAttribute('data-pinned', '');
  }
  await finalize(hub, id, blockId, 4, '\nмысль 1\nмысль 2\nмысль 3\nмысль 4');
  await expect(page.locator('.reasoning .window')).toHaveCount(0, { timeout: 5_000 });
  await expect(feed).toHaveAttribute('data-pinned', '');
  await expect.poll(() => feedGap(page)).toBeLessThanOrEqual(2);
});

test('an_automatic_fold_does_not_move_what_the_owner_reads', async ({ page }) => {
  // Окно пониже: рассуждение уходит за верх ленты, оставаясь рядом со своим смонтированным окном.
  const width = page.viewportSize()?.width ?? 412;
  await page.setViewportSize({ width, height: 600 });
  const { hub, api, id } = await boot(page, { history: 40 });
  const feed = page.locator('.feed');
  const { blockId } = startReasoning(hub, api, id);
  delta(hub, id, blockId, 1, 'Думаю над задачей\nи проверяю детали');
  hub.flush();
  await expect(page.locator('.reasoning .window')).toBeVisible();

  // Ответ после рассуждения уводит его выше окна: это отдельная карточка, как и в жизни.
  const after = api.id();
  const big = Array.from({ length: 40 }, (_, index) => `мысль большого ответа ${index}`).join('\n\n');
  hub.append(scope(id), { kind: 'block_started', conversation_id: id, payload: { ...base(api), block_id: after, rev: 0, kind: 'text' } });
  hub.append(scope(id), { kind: 'block_delta', conversation_id: id, payload: { block_id: after, base_rev: 0, rev: 1, append: big } });
  hub.flush();
  await expect(feed).toHaveAttribute('data-pinned', '');
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));

  // Поднимаемся так, чтобы карточка ответа стояла верхом чуть выше окна, а рассуждение было видно ленте.
  const viewport = await feed.evaluate((el) => el.clientHeight);
  const answer = (await page.locator('.card').last().boundingBox())!;
  await scrollUp(page, Math.max(240, answer.height - viewport - 40));
  await expect(feed).not.toHaveAttribute('data-pinned', '');
  await expect(page.locator('.reasoning')).toHaveCount(1);

  // Строка карточки, лежащая под верхом окна: её и читает владелец.
  const key = await feed.evaluate((el) => {
    const edge = el.getBoundingClientRect().top + 24;
    const rows = [...el.querySelectorAll<HTMLElement>('[data-row]')];
    const row = rows.find((item) => {
      const rect = item.getBoundingClientRect();
      return rect.top <= edge && rect.bottom > edge;
    });
    return row?.dataset.key ?? '';
  });
  expect(key).not.toBe('');
  const anchor = page.locator(`[data-row][data-key="${key}"]`);
  const before = (await anchor.boundingBox())!.y;

  await finalize(hub, id, blockId, 1, 'Думаю над задачей\nи проверяю детали');
  await expect(page.locator('.reasoning .window')).toHaveCount(0, { timeout: 5_000 });
  await page.waitForTimeout(500);
  const afterBox = await anchor.boundingBox();
  expect(afterBox).not.toBeNull();
  expect(Math.abs(afterBox!.y - before), 'anchor stays under the finger').toBeLessThanOrEqual(1);
});

for (const width of [320, 412] as const) {
  test(`reasoning_fits_without_horizontal_overflow_${width}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 800 });
    const { hub, api, id } = await boot(page);
    const { blockId } = startReasoning(hub, api, id);
    delta(hub, id, blockId, 1, `${'оченьдлинноесловобезпробелов'.repeat(12)}\nи обычная строка`);
    hub.flush();
    // Живое окно.
    await expect(page.locator('.reasoning .window')).toBeVisible();
    expect(await noOverflow(page), 'live window').toBe(true);
    // Раскрытое.
    await page.locator('.reasoning .toggle').click();
    await expect(page.locator('.reasoning .window')).toHaveCount(0);
    await expect(page.locator('.reasoning .fold')).toBeVisible();
    expect(await noOverflow(page), 'expanded').toBe(true);
    // Свёрнутое: тап владельца закрывает раскрытый блок и после финала.
    await finalize(hub, id, blockId, 1, `${'оченьдлинноесловобезпробелов'.repeat(12)}\nи обычная строка`);
    await expect(page.locator('.reasoning .window')).toHaveCount(0, { timeout: 5_000 });
    await page.locator('.reasoning .toggle').click();
    await expect(page.locator('.reasoning .preview')).toBeVisible();
    expect(await noOverflow(page), 'collapsed').toBe(true);
  });
}
