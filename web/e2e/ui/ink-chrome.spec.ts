import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

/**
 * «Чернила» в шапке чата, строке хода, очереди и композиторе: каждое движение есть только на смене
 * настоящего состояния, а при выключенном движении слово или значок меняются сразу.
 */

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

async function boot(page: Page, opts: { queued?: string } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Чернила', harness: { id: 'codex', title: 'codex', provider: 'x' } } as never);
  const items: unknown[] = [
    { kind: 'input', id: api.id(), idempotency_key: 'k-turn', state: 'accepted', content: [{ type: 'text', text: 'Покажи план' }], attachments: [], created_seq: '1', feed_seq: '1', created_at: AT }
  ];
  if (opts.queued) items.push({ kind: 'input', id: api.id(), idempotency_key: 'k-2', state: 'queued', content: [{ type: 'text', text: opts.queued }], attachments: [], created_seq: '2', feed_seq: '2', created_at: AT });
  api.histories.set(id, items as never);
  await installChatRoutes(page, hub, api, OWNER);
  await page.addInitScript(() => {
    const started: Element[] = [];
    (window as unknown as { __started: Element[] }).__started = started;
    const animate = Element.prototype.animate;
    Element.prototype.animate = function (this: Element, ...args: Parameters<Element['animate']>) {
      started.push(this);
      return animate.apply(this, args);
    };
  });
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function derived(id: string, activity: Record<string, unknown>) {
  return {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { ...activity, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 1, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  };
}

function working(hub: FakeHub, api: FakeHubApi, id: string, evidence: Record<string, unknown>) {
  const scope = `conversation:${id}`;
  const inputId = (api.histories.get(id) as { id: string }[])[0]!.id;
  hub.append(scope, derived(id, { state: 'working', evidence }));
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: inputId } } } });
  hub.flush();
  return inputId;
}

/**
 * Сколько раз на узле запускали анимацию: смена слова, складка и стрелка отправки живут в Web Animations,
 * а короткое движение опросом не поймать, поэтому запуски пишутся журналом с первого кадра страницы.
 */
const moving = (node: Locator) => node.evaluate((el) => ((window as unknown as { __started: Element[] }).__started ?? []).filter((started) => started === el).length);
const settled = (node: Locator) => node.evaluate((el) => Promise.all(el.getAnimations({ subtree: true }).map((a) => a.finished)).then(() => undefined));

test('live_line_swaps_its_label_on_a_new_occupation_and_stands_a_hammer_in_the_queue', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  working(hub, api, id, { phase: 'generating' });
  const live = page.locator('.feed .live[data-working]');
  await expect(live.locator('.label')).toHaveText('Генерирует');
  await expect(live.locator('.write')).toBeVisible();
  await expect(live.locator('.queued')).toHaveCount(0);

  // Новое занятие: слово уходит вверх и приходит новое, в том же текстовом узле.
  await page.evaluate(() => {
    const label = document.querySelector('.feed .live .label')!;
    (window as unknown as { __node: ChildNode | null }).__node = label.firstChild;
  });
  working(hub, api, id, { phase: 'running_tool', tool_name: 'Bash', summary: 'ls' });
  await expect.poll(() => moving(live.locator('.label'))).toBeGreaterThan(0);
  await expect(live.locator('.label')).toHaveText(/Выполняет/);
  await settled(live.locator('.label'));
  expect(await page.evaluate(() => (window as unknown as { __node: ChildNode | null }).__node === document.querySelector('.feed .live .label')!.firstChild)).toBe(true);
  // Часы хода тикают на месте: у них движения нет.
  expect(await moving(live.locator('.tail'))).toBe(0);

  // Очередь тяжёлых задач: перо уступает стоящему молотку, он не двигается.
  working(hub, api, id, { phase: 'running_tool', tool_name: 'Bash', summary: 'check.sh', lease_wait: { kind: 'build', position: 2, since: new Date().toISOString() } });
  await expect(live).toHaveAttribute('data-lease-wait', '');
  await expect(live.locator('.queued svg')).toBeVisible();
  await expect(live.locator('.write')).toHaveCount(0);
  await settled(live.locator('.queued'));
  expect(await moving(live.locator('.queued svg'))).toBe(0);
  // Аренда выдана: перо возвращается.
  working(hub, api, id, { phase: 'running_tool', tool_name: 'Bash', summary: 'check.sh' });
  await expect(live.locator('.write')).toBeVisible();
  await expect(live.locator('.queued')).toHaveCount(0);
});

test('live_line_and_state_chip_change_at_once_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const { hub, api, id } = await boot(page);
  working(hub, api, id, { phase: 'generating' });
  const live = page.locator('.feed .live[data-working]');
  await expect(live.locator('.label')).toHaveText('Генерирует');
  working(hub, api, id, { phase: 'running_tool', tool_name: 'Bash', summary: 'ls' });
  await expect(live.locator('.label')).toHaveText(/Выполняет/);
  expect(await moving(live.locator('.label'))).toBe(0);
  const chip = page.locator('.state-chip .state-text');
  const before = await chip.textContent();
  hub.append(`conversation:${id}`, derived(id, { state: 'waiting_for_user', evidence: { question: 'Как быть?' } }));
  hub.flush();
  await expect(chip).not.toHaveText(before!);
  expect(await moving(chip)).toBe(0);
});

test('state_chip_swaps_its_word_when_the_session_state_changes', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  working(hub, api, id, { phase: 'generating' });
  const chip = page.locator('.state-chip .state-text');
  await expect(chip).not.toHaveText('');
  await settled(chip);
  const before = (await chip.textContent())!;
  const runs = await moving(chip);
  // Смена занятия внутри работы слово шапки не трогает: это делает строка хода.
  working(hub, api, id, { phase: 'running_tool', tool_name: 'Bash', summary: 'ls' });
  await expect(page.locator('.feed .live .label')).toHaveText(/Выполняет/);
  expect(await moving(chip)).toBe(runs);
  // Сессия ждёт владельца: слово уходит вверх и приходит новое.
  hub.append(`conversation:${id}`, derived(id, { state: 'waiting_for_user', evidence: { question: 'Как быть?' } }));
  hub.flush();
  await expect.poll(() => moving(chip)).toBeGreaterThan(runs);
  await expect(chip).not.toHaveText(before);
  await settled(chip);
});

test('the_queue_folds_a_message_in_and_out_and_swaps_the_delivery_word', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  working(hub, api, id, { phase: 'generating' });
  const strip = page.locator('.strip');
  const inputId = api.id();
  hub.append(`conversation:${id}`, { kind: 'input_queued', conversation_id: id, payload: { input_id: inputId, input_seq: '20', idempotency_key: 'k-20', state: 'queued', content: [{ type: 'text', text: 'Потом собери релиз' }] } });
  hub.flush();
  const item = strip.locator('.item');
  await expect(item).toHaveCount(1);
  // Сообщение встаёт в очередь складкой, а не выскакивает.
  expect(await moving(item)).toBeGreaterThan(0);
  await settled(item);
  const label = item.locator('.label').first();
  await expect(label).toHaveText('В очереди');
  hub.append(`conversation:${id}`, { kind: 'input_state_changed', conversation_id: id, payload: { input_id: inputId, state: 'dispatched', attempt: 1 } });
  hub.flush();
  await expect.poll(() => moving(label)).toBeGreaterThan(0);
  await expect(label).toHaveText('На машине');
  await settled(label);
  // Уходит тоже складкой: узел ещё в документе, пока высота сходится. Масштаб движения растянут, чтобы успеть увидеть складку.
  await page.evaluate(() => document.documentElement.style.setProperty('--motion-scale', '8'));
  const leaving = (await item.elementHandle())!;
  const runs = await leaving.evaluate((el) => ((window as unknown as { __started: Element[] }).__started).filter((started) => started === el).length);
  hub.append(`conversation:${id}`, { kind: 'input_state_changed', conversation_id: id, payload: { input_id: inputId, state: 'accepted' } });
  hub.flush();
  await expect.poll(() => leaving.evaluate((el, before) => ((window as unknown as { __started: Element[] }).__started).filter((started) => started === el).length > before, runs)).toBe(true);
  await expect(item).toHaveCount(0);
});

test('queue_motion_is_off_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const { hub, api, id } = await boot(page);
  working(hub, api, id, { phase: 'generating' });
  hub.append(`conversation:${id}`, { kind: 'input_queued', conversation_id: id, payload: { input_id: api.id(), input_seq: '20', idempotency_key: 'k-20', state: 'queued', content: [{ type: 'text', text: 'Потом собери релиз' }] } });
  hub.flush();
  const item = page.locator('.strip .item');
  await expect(item).toHaveCount(1);
  expect(await moving(item)).toBe(0);
});

test('send_launches_its_arrow_on_tap_and_stays_still_on_enter', async ({ page }, info) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  await area.fill('Привет');
  await send.click();
  await expect.poll(() => moving(send.locator('svg'))).toBeGreaterThan(0);
  await expect(area).toHaveValue('');
  await settled(send);
  // Enter с клавиатуры отправляет мгновенно (на сенсорном экране он вставляет перенос и ничего не шлёт).
  if (info.project.name === 'desktop') {
    const runs = await moving(send.locator('svg'));
    await area.fill('Ещё раз');
    await area.press('Enter');
    await expect(area).toHaveValue('');
    expect(await moving(send.locator('svg'))).toBe(runs);
  }
});

test('send_arrow_does_not_move_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  const send = page.getByRole('button', { name: 'Отправить', exact: true });
  await area.fill('Привет');
  await send.click();
  await expect(area).toHaveValue('');
  expect(await moving(send.locator('svg'))).toBe(0);
});

test('bookmark_fill_fades_in_instead_of_snapping', async ({ page }) => {
  const { api, id } = await boot(page);
  api.histories.set(id, [{ kind: 'input', id: api.id(), idempotency_key: 'k-1', state: 'accepted', content: [{ type: 'text', text: 'Запомни это' }], attachments: [], created_seq: '1', feed_seq: '1', created_at: AT }] as never);
  api.bindConversation(id);
  await page.reload();
  const bookmark = page.locator('article.bubble').getByRole('button', { name: 'В избранное' });
  // Масштаб движения растянут: переход заливки успевает попасться на глаза проверке.
  await page.evaluate(() => {
    document.documentElement.style.setProperty('--motion-scale', '8');
    const seen: string[] = [];
    (window as unknown as { __runs: string[] }).__runs = seen;
    document.addEventListener('transitionrun', (event) => seen.push(`${(event.target as Element).tagName}:${event.propertyName}`), true);
  });
  await bookmark.click();
  const filled = page.locator('article.bubble').getByRole('button', { name: 'Убрать из избранного' }).locator('svg');
  await expect(filled).toHaveClass(/filled/);
  // Заливка проявляется переходом на значке, а не вспыхивает.
  expect(await page.evaluate(() => (window as unknown as { __runs: string[] }).__runs)).toContain('svg:fill');
});

test('bookmark_fill_is_instant_when_motion_is_reduced', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const { api, id } = await boot(page);
  api.histories.set(id, [{ kind: 'input', id: api.id(), idempotency_key: 'k-1', state: 'accepted', content: [{ type: 'text', text: 'Запомни это' }], attachments: [], created_seq: '1', feed_seq: '1', created_at: AT }] as never);
  api.bindConversation(id);
  await page.reload();
  const runs = await page.evaluate(() => {
    const seen: string[] = [];
    (window as unknown as { __runs: string[] }).__runs = seen;
    document.addEventListener('transitionrun', (event) => seen.push(`${(event.target as Element).tagName}:${event.propertyName}`), true);
    return seen;
  });
  expect(runs).toEqual([]);
  await page.locator('article.bubble').getByRole('button', { name: 'В избранное' }).click();
  const filled = page.locator('article.bubble').getByRole('button', { name: 'Убрать из избранного' }).locator('svg');
  await expect(filled).toHaveClass(/filled/);
  expect(await page.evaluate(() => (window as unknown as { __runs: string[] }).__runs)).not.toContain('svg:fill');
});
