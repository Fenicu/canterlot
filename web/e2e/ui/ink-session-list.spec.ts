import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions, type MockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

declare global {
  interface Window {
    __animated?: { role: string | null; cls: string; props: string[]; text: string }[];
  }
}

/** Все вызовы `element.animate` с тем, что они двигают: так видно, где движение было, а где слово сменилось сразу. */
async function recordAnimations(page: Page) {
  await page.addInitScript(() => {
    const log: NonNullable<Window['__animated']> = [];
    window.__animated = log;
    const original = Element.prototype.animate;
    Element.prototype.animate = function (this: Element, keyframes: Keyframe[] | PropertyIndexedKeyframes | null, options?: number | KeyframeAnimationOptions) {
      const frames = Array.isArray(keyframes) ? keyframes : [];
      log.push({ role: this.getAttribute('role'), cls: this.getAttribute('class') ?? '', props: [...new Set(frames.flatMap((frame) => Object.keys(frame)))], text: this.textContent ?? '' });
      return original.call(this, keyframes as Keyframe[], options);
    };
  });
}

const animated = (page: Page, cls: string) => page.evaluate((name) => window.__animated!.filter((entry) => entry.cls.split(' ').includes(name)).length, cls);

async function boot(page: Page, extra = 0) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api) as MockupSessions;
  const crowd: string[] = [];
  for (let index = 0; index < extra; index += 1) {
    crowd.push(seedConversation(api, {
      title: `Работает ${index}`,
      machine: { id: api.id(), name: 'workstation' },
      harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
      model: 'opus-5.5',
      activity: { state: 'working', evidence: { phase: 'generating' }, since: AT }
    }));
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids, crowd };
}

/** Хаб присылает новое состояние сессии: слово и счётчик меняются на глазах. */
function derive(hub: FakeHub, api: FakeHubApi, id: string, patch: { activity?: Record<string, unknown>; unread_count?: number }): void {
  const row = api.list.get(id)!;
  if (patch.activity) row.activity = patch.activity as never;
  if (patch.unread_count !== undefined) row.unread_count = patch.unread_count;
  hub.append('conversations', {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: row.activity,
      unread_count: row.unread_count,
      queue: row.queue ?? { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: row.last_activity_at ?? null,
      settings: { model: row.model ?? 'opus', effort: row.effort ?? 'medium', permission_mode: row.permission_mode ?? 'owner' },
      crunch: { enabled: row.crunch?.enabled ?? false }
    }
  });
  hub.flush();
}

const rowOf = (page: Page, id: string) => page.locator(`[data-session="${id}"]`);
const breathing = (page: Page) => page.evaluate(() => document.getAnimations().filter((item) => (item as CSSAnimation).animationName.endsWith('avatar-breathe')).length);

test('working_ring_breathes_and_waiting_or_failed_rings_stay_solid', async ({ page }) => {
  const { ids, api } = await boot(page);
  const ring = rowOf(page, ids.pinned).locator('.avatar');
  await expect(ring).toHaveAttribute('data-breathe', '');
  await expect.poll(() => breathing(page)).toBeGreaterThan(0);
  // Ждёт владельца, подвисла, пауза: кольцо просит взгляда и не пульсирует.
  for (const [id, row] of api.list) {
    if (id === ids.pinned) continue;
    if (row.activity.state === 'working') continue;
    await expect(rowOf(page, id).locator('.avatar')).not.toHaveAttribute('data-breathe', '');
  }
  const solid = await rowOf(page, ids.pinned).locator('.avatar').evaluate((node) => getComputedStyle(node).outlineStyle);
  expect(['none', 'solid']).toContain(solid);
});

test('breathing_is_capped_to_the_rows_in_view', async ({ page }) => {
  const { crowd } = await boot(page, 40);
  const list = page.locator('[data-list-scroll]');
  await expect(rowOf(page, crowd[0]!)).toBeVisible();
  const count = await page.locator('[data-breathe]').count();
  // Сорок один работающий ряд, видны единицы: дышат только они.
  expect(count).toBeGreaterThan(0);
  expect(count).toBeLessThan(25);
  const outside = await page.locator('[data-breathe]').evaluateAll((nodes) => nodes.filter((node) => {
    const box = node.getBoundingClientRect();
    return box.bottom < 0 || box.top > window.innerHeight;
  }).length);
  expect(outside).toBe(0);
  await list.evaluate((node) => { node.scrollTop = node.scrollHeight; });
  await expect.poll(() => page.locator('[data-breathe]').evaluateAll((nodes) => nodes.every((node) => {
    const box = node.getBoundingClientRect();
    return box.bottom >= 0 && box.top <= window.innerHeight;
  }))).toBe(true);
  // Хвост списка прокручен в окно: у его рядов кольцо дышит, у ушедших вверх нет.
  expect(await page.locator('[data-breathe]').count()).toBeLessThan(25);
  await expect.poll(() => breathing(page)).toBe(await page.locator('[data-breathe]').count());
});

for (const motion of ['reduced', 'off'] as const) {
  test(`ring_and_placeholders_stand_still_${motion}`, async ({ page }) => {
    if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
    else await page.addInitScript(() => localStorage.setItem('canterlot.motion', 'off'));
    const { ids } = await boot(page);
    const ring = rowOf(page, ids.pinned).locator('.avatar');
    await expect(ring).toBeVisible();
    expect(await breathing(page)).toBe(0);
    expect(await ring.evaluate((node) => getComputedStyle(node, '::before').animationDuration)).toBe('0s');
    // Заготовка загрузки: тот же класс, что у всех мест ожидания.
    await page.evaluate(() => {
      const bar = document.createElement('span');
      bar.className = 'skeleton';
      bar.id = 'probe-skeleton';
      document.body.append(bar);
    });
    expect(await page.locator('#probe-skeleton').evaluate((node) => getComputedStyle(node).animationDuration)).toBe('0s');
    expect(await page.evaluate(() => document.getAnimations().filter((item) => (item as CSSAnimation).animationName === 'skeleton-breathe').length)).toBe(0);
  });
}

test('loading_placeholders_breathe_on_opacity_only', async ({ page }) => {
  await boot(page);
  await page.evaluate(() => {
    const bar = document.createElement('span');
    bar.className = 'skeleton';
    bar.id = 'probe-skeleton';
    document.body.append(bar);
  });
  const bar = page.locator('#probe-skeleton');
  expect(await bar.evaluate((node) => getComputedStyle(node).animationName)).toBe('skeleton-breathe');
  await expect.poll(() => page.evaluate(() => document.getAnimations().filter((item) => (item as CSSAnimation).animationName === 'skeleton-breathe').length)).toBe(1);
  const props = await page.evaluate(() => {
    const animation = document.getAnimations().find((item) => (item as CSSAnimation).animationName === 'skeleton-breathe')!;
    return [...new Set((animation.effect as KeyframeEffect).getKeyframes().flatMap((frame) => Object.keys(frame)))].filter((key) => !['offset', 'easing', 'composite', 'computedOffset'].includes(key));
  });
  expect(props).toEqual(['opacity']);
});

test('status_word_swaps_only_when_the_state_changes_and_keeps_its_text_node', async ({ page }) => {
  await recordAnimations(page);
  const { hub, api, ids } = await boot(page);
  const text = rowOf(page, ids.pinned).locator('.activity .text');
  await expect(text).toHaveText('Генерирует');
  const node = await text.evaluateHandle((element) => element.firstChild);
  const before = await animated(page, 'text');
  // Тот же ход, другой инструмент: слово правится на месте без движения.
  derive(hub, api, ids.pinned, { activity: { state: 'working', evidence: { phase: 'running_tool', tool_name: 'cargo test', phase_since: AT }, since: AT } });
  await expect(text).toContainText('cargo test');
  expect(await animated(page, 'text')).toBe(before);
  // Теперь сессия ждёт владельца: слово уходит вверх и приходит новое.
  derive(hub, api, ids.pinned, { activity: { state: 'waiting_for_user', evidence: { phase: 'awaiting_permission', question: 'Bash: cargo test' }, since: AT } });
  await expect(text).toContainText('Ждёт разрешения');
  expect(await animated(page, 'text')).toBeGreaterThanOrEqual(before + 2);
  expect(await text.evaluate((element, old) => element.firstChild === old, node)).toBe(true);
  await expect(rowOf(page, ids.pinned).locator('.avatar')).not.toHaveAttribute('data-breathe', '');
});

test('status_word_is_replaced_at_once_under_reduced_motion', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await recordAnimations(page);
  const { hub, api, ids } = await boot(page);
  const text = rowOf(page, ids.pinned).locator('.activity .text');
  await expect(text).toHaveText('Генерирует');
  derive(hub, api, ids.pinned, { activity: { state: 'waiting_for_user', evidence: { phase: 'awaiting_permission', question: 'Bash: cargo test' }, since: AT } });
  await expect(text).toContainText('Ждёт разрешения');
  expect(await animated(page, 'text')).toBe(0);
});

test('unread_counter_swaps_when_its_number_changes', async ({ page }) => {
  await recordAnimations(page);
  const { hub, api, ids } = await boot(page);
  const count = rowOf(page, ids.pinned).locator('.count');
  await expect(count).toHaveText('1');
  const before = await animated(page, 'count');
  derive(hub, api, ids.pinned, { unread_count: 3 });
  await expect(count).toHaveText('3');
  expect(await animated(page, 'count')).toBeGreaterThanOrEqual(before + 2);
});

test('section_and_tab_counters_swap_when_their_number_changes', async ({ page }) => {
  await recordAnimations(page);
  const { hub, api, ids } = await boot(page);
  const head = page.locator('h2 .count').first();
  await expect(head).toBeVisible();
  const before = await animated(page, 'count');
  derive(hub, api, ids.plain, { activity: { state: 'waiting_for_user', evidence: { question: 'Какой вариант?' }, since: AT } });
  await expect.poll(() => animated(page, 'count')).toBeGreaterThan(before);
});

test('counters_are_replaced_at_once_under_reduced_motion', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await recordAnimations(page);
  const { hub, api, ids } = await boot(page);
  const count = rowOf(page, ids.pinned).locator('.count');
  await expect(count).toHaveText('1');
  derive(hub, api, ids.pinned, { unread_count: 4 });
  await expect(count).toHaveText('4');
  expect(await animated(page, 'count')).toBe(0);
});
