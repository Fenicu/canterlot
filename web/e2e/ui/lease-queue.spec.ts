import type { Page, TestInfo } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, leaseEntry, seedConversation, seedLease } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';
import { foldingCount, movingCount, noMotion, slowMotion } from './ink-motion';
import { bootSecondary } from './secondary-fixture';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const ago = (ms: number): string => new Date(Date.now() - ms).toISOString();
const MIN = 60_000;

/** Снимки для отчёта: телефон 390×844 в тёмной теме, рабочий стол 1280×800 в светлой. */
async function shoot(page: Page, testInfo: TestInfo, name: string) {
  if (!process.env.LEASE_UI_SCREENSHOTS) return;
  const { mkdirSync } = await import('node:fs');
  mkdirSync('/tmp/lease-ui', { recursive: true });
  // Экран машины прокручивается внутри своего контейнера: секцию очереди нужно подвести к виду.
  if (name === 'machines') await page.locator('[data-lease-queue]').scrollIntoViewIfNeeded();
  await page.screenshot({ path: `/tmp/lease-ui/${testInfo.project.name}-${name}.png` });
}

async function frame(page: Page, testInfo: TestInfo) {
  const phone = testInfo.project.name === 'phone';
  await page.emulateMedia({ colorScheme: phone ? 'dark' : 'light' });
  await page.addInitScript((theme) => localStorage.setItem('canterlot.theme', theme), phone ? 'dark' : 'light');
  await page.setViewportSize(phone ? { width: 390, height: 844 } : { width: 1280, height: 800 });
}

test('machine_screen_lists_queue_holders_and_waiters_and_hides_an_empty_or_stale_queue', async ({ page }, testInfo) => {
  await frame(page, testInfo);
  const { hub, ids } = await bootSecondary(page, '/machines', (api) => {
    const plain = [...api.list.values()].find((row) => row.title === 'Очередь сообщений')!.id;
    seedLease(api, [...api.machines.values()].find((machine) => machine.name === 'workstation')!.id, [
      leaseEntry({ lease_id: 'h1', kind: 'build', conversation_id: [...api.list.values()].find((row) => row.title === 'Восстановление потока')!.id, since: ago(12 * MIN) }),
      leaseEntry({ lease_id: 'h2', kind: 'build', label: 'check.sh --fast', dir: 'canterlot', since: ago(47 * MIN) })
    ], [
      leaseEntry({ lease_id: 'w1', kind: 'gate', conversation_id: plain, requested_at: ago(5 * MIN), position: 1 }),
      leaseEntry({ lease_id: 'w2', kind: 'build', label: 'npm run build', dir: 'canterlot-2a', requested_at: ago(2 * MIN), position: 2 })
    ]);
  });
  const main = page.locator('main');
  const section = main.locator('[data-lease-queue]');
  await expect(section.getByRole('heading', { name: 'Очередь тяжёлых задач' })).toBeVisible();
  const holders = section.locator('[data-lease-holder]');
  await expect(holders).toHaveCount(2);
  await expect(holders.nth(0)).toContainText('Восстановление потока');
  await expect(holders.nth(0)).toContainText('сборка · 12 мин');
  await expect(holders.nth(0).getByRole('link', { name: 'Восстановление потока' })).toHaveAttribute('href', `/c/${ids.pinned}`);
  // Держатель без метки исполнения — терминал владельца.
  await expect(holders.nth(1)).toContainText('вы');
  await expect(holders.nth(1)).toContainText('сборка · 47 мин');
  const waiters = section.locator('[data-lease-waiter]');
  await expect(waiters).toHaveCount(2);
  await expect(waiters.nth(0)).toContainText('Очередь сообщений');
  await expect(waiters.nth(0)).toContainText('1-й · гейт');
  await expect(waiters.nth(1)).toContainText('вы');
  await expect(waiters.nth(1)).toContainText('2-й · сборка');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  await shoot(page, testInfo, 'machines');

  // Пустая очередь не показывается.
  hub.append('machines', { kind: 'machine_lease_changed', payload: { machine_id: ids.machine, lease: { slots: 2, observed_at: new Date().toISOString(), holders: [], waiters: [] } } }); hub.flush();
  await expect(section).toHaveCount(0);
  // Новое состояние приходит событием.
  hub.append('machines', { kind: 'machine_lease_changed', payload: { machine_id: ids.machine, lease: { slots: 2, observed_at: new Date().toISOString(), holders: [leaseEntry({ lease_id: 'g', kind: 'gate', since: ago(3 * MIN) })], waiters: [] } } }); hub.flush();
  await expect(section.locator('[data-lease-holder]')).toContainText('гейт · 3 мин');
  // Пропавшая машина не подтверждает очередь: секция уходит.
  hub.append('machines', { kind: 'machine_availability_changed', payload: { machine_id: ids.machine, availability: 'unavailable' } }); hub.flush();
  await expect(section).toHaveCount(0);
});

/** Очередь с двумя держателями и одним ожидающим: на машине рабочей станции. */
async function seededQueue(page: Page) {
  return bootSecondary(page, '/machines', (api) => {
    seedLease(api, [...api.machines.values()].find((machine) => machine.name === 'workstation')!.id, [
      leaseEntry({ lease_id: 'h1', kind: 'build', label: 'cargo build', dir: 'canterlot', since: ago(12 * MIN) }),
      leaseEntry({ lease_id: 'h2', kind: 'build', label: 'check.sh --fast', dir: 'canterlot', since: ago(47 * MIN) })
    ], [
      leaseEntry({ lease_id: 'w1', kind: 'gate', label: 'gate', requested_at: ago(5 * MIN), position: 1 })
    ]);
  });
}

const queueNow = (hub: FakeHub, machine: string, holders: unknown[], waiters: unknown[] = []) => {
  hub.append('machines', { kind: 'machine_lease_changed', payload: { machine_id: machine, lease: { slots: 2, observed_at: new Date().toISOString(), holders, waiters } } });
  hub.flush();
};

test('queue_rows_carry_the_pen_for_what_runs_and_an_hourglass_for_what_waits', async ({ page }, testInfo) => {
  await frame(page, testInfo);
  await seededQueue(page);
  const section = page.locator('[data-lease-queue]');
  const holders = section.locator('[data-lease-holder]');
  await expect(holders).toHaveCount(2);
  for (const holder of await holders.all()) {
    await expect(holder.locator('[data-lease-mark="writing"] .write')).toHaveCount(1);
    // Перо пишет по-настоящему: у штриха идёт анимация.
    expect(await holder.locator('.write i').first().evaluate((node) => getComputedStyle(node).animationName)).not.toBe('none');
  }
  const waiter = section.locator('[data-lease-waiter]');
  await expect(waiter.locator('[data-lease-mark="waiting"] svg')).toHaveCount(1);
  await expect(waiter.locator('.write')).toHaveCount(0);
  // Ожидающему ничего не пишется, песочные часы стоят.
  expect(await waiter.locator('[data-lease-mark="waiting"] svg').evaluate((node) => node.getAnimations({ subtree: true }).length)).toBe(0);
});

test('queue_rows_fold_when_they_join_or_leave_and_the_rest_stay', async ({ page }, testInfo) => {
  await frame(page, testInfo);
  const { hub, ids } = await seededQueue(page);
  const section = page.locator('[data-lease-queue]');
  await slowMotion(page);
  const only = leaseEntry({ lease_id: 'h1', kind: 'build', label: 'cargo build', dir: 'canterlot', since: ago(12 * MIN) });
  // Держатель ушёл: его строка складывается, а не пропадает кадром.
  queueNow(hub, ids.machine, [only], [leaseEntry({ lease_id: 'w1', kind: 'gate', label: 'gate', requested_at: ago(5 * MIN), position: 1 })]);
  await expect.poll(() => foldingCount(page, '[data-lease-holder]')).toBeGreaterThan(0);
  // Складываемая строка уже след: диктору не читается.
  await expect(section.locator('[data-lease-holder][aria-hidden="true"]')).toHaveCount(1);
  await expect(section.locator('[data-lease-holder]:not([aria-hidden])')).toHaveCount(1);
  // Новый ожидающий приходит складкой.
  queueNow(hub, ids.machine, [only], [
    leaseEntry({ lease_id: 'w1', kind: 'gate', label: 'gate', requested_at: ago(5 * MIN), position: 1 }),
    leaseEntry({ lease_id: 'w2', kind: 'build', label: 'npm run build', dir: 'canterlot-2a', requested_at: ago(MIN), position: 2 })
  ]);
  await expect.poll(() => foldingCount(page, '[data-lease-waiter]')).toBeGreaterThan(0);
  await expect(section.locator('[data-lease-waiter]:not([aria-hidden])')).toHaveCount(2);
});

test('queue_stands_still_under_reduced_motion_and_motion_scale_zero', async ({ page }, testInfo) => {
  await frame(page, testInfo);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const { hub, ids } = await seededQueue(page);
  const section = page.locator('[data-lease-queue]');
  await expect(section.locator('[data-lease-holder]')).toHaveCount(2);
  // Перо стоит написанным: ни одного штриха с анимацией.
  expect(await section.locator('.write i').evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).animationName))).toEqual(['none', 'none', 'none', 'none', 'none', 'none']);
  queueNow(hub, ids.machine, [leaseEntry({ lease_id: 'h1', kind: 'build', label: 'cargo build', dir: 'canterlot', since: ago(12 * MIN) })]);
  // Строка уходит сразу: ни складки, ни следа.
  await expect(section.locator('[data-lease-holder]')).toHaveCount(1);
  await expect(section.locator('[data-lease-waiter]')).toHaveCount(0);
  expect(await movingCount(page, '[data-lease-queue]')).toBe(0);
  // То же при масштабе движения 0.
  await noMotion(page);
  queueNow(hub, ids.machine, [], [leaseEntry({ lease_id: 'w9', kind: 'gate', label: 'gate', requested_at: ago(MIN), position: 1 })]);
  await expect(section.locator('[data-lease-holder]')).toHaveCount(0);
  await expect(section.locator('[data-lease-waiter]')).toHaveCount(1);
  expect(await movingCount(page, '[data-lease-queue]')).toBe(0);
});

async function waitingChat(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Баннер связи', model: 'opus-5.5' });
  const blockId = api.id();
  const command = api.id();
  api.blocks.set(blockId, { text: '', rev: 1, state: 'streaming' });
  const item: HistoryBlock = {
    kind: 'block', id: blockId, block_kind: 'tool_call', content: '{"command":"scripts/check.sh --fast"}', content_bytes: 37, content_truncated: false,
    created_seq: '1', created_at: ago(90_000), rev: 1, state: 'streaming', agent: 'claude', model: 'opus-5.5',
    tool_name: 'Bash', summary: 'scripts/check.sh --fast', parent_block_id: null, input_command_id: command, line_count: 1
  };
  api.histories.set(id, [item]);
  api.bindConversation(id);
  api.details.get(id)!.activity = {
    state: 'working',
    evidence: { phase: 'running_tool', tool_name: 'Bash', summary: 'scripts/check.sh --fast', phase_since: ago(90_000), lease_wait: { kind: 'build', position: 2, since: ago(80_000) } },
    since: ago(90_000)
  };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, id };
}

function derived(id: string, evidence: Record<string, unknown>) {
  return {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence, since: ago(90_000) },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: null,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  };
}

test('chat_live_line_of_a_waiting_session_names_the_queue_and_its_place', async ({ page }, testInfo) => {
  await frame(page, testInfo);
  const { hub, id } = await waitingChat(page);
  const live = page.locator('.feed .live[data-working]');
  await expect(live).toHaveText(/Ждёт очереди на сборку · 2-й/);
  await expect(live).toHaveAttribute('data-lease-wait', '');
  await shoot(page, testInfo, 'chat');

  // Очередь продвинулась и сменился вид: строка следует за свидетельством.
  hub.append(`conversation:${id}`, derived(id, { phase: 'running_tool', tool_name: 'Bash', summary: 'scripts/check.sh --all', lease_wait: { kind: 'gate', position: 1, since: ago(10_000) } })); hub.flush();
  await expect(live).toHaveText(/Ждёт очереди на гейт · 1-й/);
  // Аренда выдана: снова видно, что выполняется.
  hub.append(`conversation:${id}`, derived(id, { phase: 'running_tool', tool_name: 'Bash', summary: 'scripts/check.sh --all' })); hub.flush();
  await expect(live).not.toHaveText(/Ждёт очереди/);
  await expect(live).not.toHaveAttribute('data-lease-wait');
});

test('session_row_status_of_a_waiting_session_names_the_queue', async ({ page }, testInfo) => {
  await frame(page, testInfo);
  const { hub, ids } = await bootSecondary(page, '/sessions');
  const row = page.locator('[data-pane="list"], main').getByRole('link', { name: /Восстановление потока/ }).first();
  await expect(row).toBeVisible();
  hub.append('conversations', derived(ids.pinned, { phase: 'running_tool', tool_name: 'Bash', lease_wait: { kind: 'build', position: 3, since: ago(60_000) } })); hub.flush();
  await expect(row).toContainText('Ждёт очереди на сборку · 3-й');
});
