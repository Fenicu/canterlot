import type { Locator, Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { ScheduledMessage } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

/**
 * Мелкая находка ревью от 2026-11-16: F-20.
 * Очередь меряется только там, где включается её телефонная раскладка (`pointer: coarse`).
 */
const OWNER = '00000000-0000-4000-8000-0000000000d1';
const WIDTHS = [360, 412];
const SHOTS = process.env.W3_SHOTS ? '/tmp/w3-queue' : null;

const phoneOnly = (info: { project: { name: string } }) => test.skip(info.project.name !== 'phone', 'раскладка очереди с «⋯» живёт под pointer: coarse');

interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

async function boot(page: Page, width: number, seed?: (api: FakeHubApi, id: string) => void): Promise<{ hub: FakeHub; api: FakeHubApi; id: string }> {
  await page.setViewportSize({ width, height: 892 });
  await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'dark'));
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  api.bindConversation(id);
  seed?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function seedScheduled(api: FakeHubApi, id: string, rows: { text: string }[]): void {
  const snap = api.conversationData(id);
  snap.scheduled = rows.map((row, index) => ({
    id: api.id(),
    conversation_id: id,
    run_at: new Date(Date.now() + 60 * 60_000 * (index + 1)).toISOString(),
    content: [{ type: 'text', text: row.text }],
    attachment_ids: [],
    state: 'pending',
    input_id: null,
    attempts: 0
  })) as ScheduledMessage[];
  api.hub.setSnapshot(`conversation:${id}`, () => snap);
}

async function queue(hub: FakeHub, api: FakeHubApi, id: string, texts: string[]): Promise<void> {
  texts.forEach((text, index) =>
    hub.append(`conversation:${id}`, {
      kind: 'input_queued',
      conversation_id: id,
      payload: { input_id: api.id(), input_seq: String(index + 20), idempotency_key: api.id(), state: 'queued', content: [{ type: 'text', text }] }
    })
  );
  hub.flush();
}

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  await page.locator('.strip').first().screenshot({ path: join(SHOTS, `${name}.png`) });
}

async function boxes(pill: Locator): Promise<{ pill: Box; body: Box; line: number; target: number; button: Box; icon: Box }> {
  return pill.evaluate((node) => {
    const box = (el: Element): Box => {
      const rect = el.getBoundingClientRect();
      return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height };
    };
    const body = node.querySelector('.body') as HTMLElement;
    const bodyStyle = getComputedStyle(body);
    const line = Number.parseFloat(bodyStyle.lineHeight) || Number.parseFloat(bodyStyle.fontSize) * 1.4;
    const button = node.querySelector('.row-actions .overflow') as HTMLElement;
    const icon = button.querySelector('svg') as SVGSVGElement;
    return {
      pill: box(node),
      body: box(body),
      line,
      target: Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--target')),
      button: box(button),
      icon: box(icon)
    };
  });
}

/** «⋯» стоит по центру первой строки, а цель касания не выходит за карточку. */
async function expectOverflowOnFirstLine(pill: Locator, label: string): Promise<void> {
  const found = await boxes(pill);
  const firstLineCentre = found.body.top + found.line / 2;
  const iconCentre = found.icon.top + found.icon.height / 2;
  expect(Math.abs(iconCentre - firstLineCentre), `${label}: «⋯» по центру первой строки`).toBeLessThanOrEqual(1);
  expect(found.button.height, `${label}: цель «⋯» не ниже --target`).toBeGreaterThanOrEqual(found.target - 0.5);
  expect(found.button.width, `${label}: цель «⋯» не уже --target`).toBeGreaterThanOrEqual(found.target - 0.5);
  expect(found.button.top, `${label}: цель не выходит над карточкой`).toBeGreaterThanOrEqual(found.pill.top - 0.5);
  expect(found.button.bottom, `${label}: цель не выходит под карточкой`).toBeLessThanOrEqual(found.pill.bottom + 0.5);
  expect(found.button.left, `${label}: цель не выходит слева`).toBeGreaterThanOrEqual(found.pill.left - 0.5);
  expect(found.button.right, `${label}: цель не выходит справа`).toBeLessThanOrEqual(found.pill.right + 0.5);
  // Точка внутри цели принадлежит кнопке, а не соседнему элементу.
  const hit = await pill.evaluate((node) => {
    const button = node.querySelector('.row-actions .overflow') as HTMLElement;
    const rect = button.getBoundingClientRect();
    const points = [
      [rect.left + rect.width / 2, rect.top + rect.height / 2],
      [rect.left + rect.width / 2, rect.top + 2],
      [rect.left + rect.width / 2, rect.bottom - 2]
    ];
    return points.map(([x, y]) => {
      const found = document.elementFromPoint(x!, y!);
      return { x, y, own: found !== null && (found === button || button.contains(found)) };
    });
  });
  expect(hit.filter((point) => !point.own), `${label}: точки цели принадлежат «⋯»`).toEqual([]);
}

const TWO_LINES = 'После тестов обнови README и добавь строку про порядок подписки и снимка журнала';

for (const width of WIDTHS) {
  test(`queue_overflow_sits_on_the_first_text_line ${width}`, async ({ page }, info) => {
    phoneOnly(info);
    await boot(page, width, (a, conversation) => {
      seedScheduled(a, conversation, [{ text: 'позже' }, { text: TWO_LINES }]);
    });
    await expect(page.locator('.strip .pill')).toHaveCount(2);
    await expect(page.locator('.strip .pill').nth(0)).toBeVisible();
    await expectOverflowOnFirstLine(page.locator('.strip .pill').nth(0), `отложенное в одну строку @${width}`);
    await expectOverflowOnFirstLine(page.locator('.strip .pill').nth(1), `отложенное в две строки @${width}`);
    await shot(page, `f20-scheduled-${width}`);
  });

  test(`queued_card_overflow_sits_on_the_first_text_line ${width}`, async ({ page }, info) => {
    phoneOnly(info);
    const { hub, api, id } = await boot(page, width);
    await queue(hub, api, id, ['Третье', TWO_LINES]);
    const queued = page.locator('.strip [data-state] .pill');
    await expect(queued).toHaveCount(2);
    await expectOverflowOnFirstLine(queued.nth(0), `очередь в одну строку @${width}`);
    await expectOverflowOnFirstLine(queued.nth(1), `очередь в две строки @${width}`);
    await shot(page, `f20-queue-${width}`);
  });
}
