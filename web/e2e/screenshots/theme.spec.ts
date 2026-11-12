import { test, expect, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const OUT = resolve('test-results/themes');
const VARIANTS = [
  { palette: 'aquamarine', theme: 'dark', key: 'aquamarine-dark' },
  { palette: 'aquamarine', theme: 'light', key: 'aquamarine-light' },
  { palette: 'standard', theme: 'dark', key: 'standard-dark' },
  { palette: 'standard', theme: 'light', key: 'standard-light' },
  { palette: 'vscode', theme: 'dark', key: 'vscode-dark' },
  { palette: 'vscode', theme: 'light', key: 'vscode-light' }
] as const;

test.use({ viewport: { width: 412, height: 892 }, isMobile: true, hasTouch: true });
test.describe.configure({ mode: 'serial' });

function historyBlock(api: FakeHubApi, run: string, rest: Partial<HistoryBlock> & Pick<HistoryBlock, 'block_kind' | 'content'>): HistoryBlock {
  return {
    kind: 'block', id: api.id(), content_bytes: rest.content.length, content_truncated: false,
    created_seq: '2', created_at: AT, rev: 1, state: 'final', agent: 'Claude Code', model: 'opus-5.5',
    tool_name: null, summary: null, parent_block_id: null, input_command_id: run, line_count: rest.content.split('\n').length,
    ...rest
  };
}

function seedChat(api: FakeHubApi): string {
  seedMachine(api, {
    id: 'workstation', name: 'workstation',
    harnesses: [{ id: 'claude', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }], permission_modes: ['owner', 'ask', 'bypass', 'read_only'] }]
  });
  const id = seedConversation(api, { title: 'Проверка темы', model: 'opus-5.5', effort: 'medium', permission_mode: 'owner', activity: { state: 'idle', evidence: {}, since: AT } });
  const detail = api.details.get(id)!;
  detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  detail.machine = { id: 'workstation', name: 'workstation' };
  const run = api.id();
  const user: HistoryInput = { kind: 'input', id: api.id(), idempotency_key: 'theme-user', state: 'accepted', content: [{ type: 'text', text: 'Покажи токены и вывод тестов' }], attachments: [], created_seq: '1', feed_seq: '1', created_at: AT };
  const answer = historyBlock(api, run, {
    block_kind: 'text', content: 'Синтаксис берёт цвета из варианта палитры.\n\n```ts\nexport type Palette = \'aquamarine\' | \'standard\' | \'vscode\';\nconst active: Palette = \'standard\';\n```\n\nПроверяю вывод.',
    created_seq: '2'
  });
  const tool = historyBlock(api, run, {
    block_kind: 'tool_call', content: 'test result: ok. 880 passed; 0 failed\nok   theme\nok   contrast', tool_name: 'Bash',
    summary: 'npm test', created_seq: '3'
  });
  api.histories.set(id, [user, answer, tool]);
  api.blocks.set(answer.id, { text: answer.content, rev: 1, state: 'final' });
  api.blocks.set(tool.id, { text: tool.content, rev: 1, state: 'final' });
  api.bindConversation(id);
  return id;
}

async function setVariant(page: Page, palette: string, theme: string): Promise<void> {
  await page.evaluate(({ palette, theme }) => {
    localStorage.setItem('canterlot.palette', palette);
    localStorage.setItem('canterlot.theme', theme);
  }, { palette, theme });
  await page.reload();
}

async function save(page: Page, screen: string, key: string): Promise<void> {
  await page.waitForTimeout(180);
  await page.screenshot({ path: resolve(OUT, `${screen}-${key}.png`), animations: 'disabled' });
}

test.beforeAll(() => { mkdirSync(OUT, { recursive: true }); });

test('sessions_for_every_variant', async ({ page }) => {
  const hub = new FakeHub(); const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  for (const variant of VARIANTS) {
    await setVariant(page, variant.palette, variant.theme);
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await save(page, 'sessions', variant.key);
  }
});

test('chat_with_code_and_tool_output_for_every_variant', async ({ page }) => {
  const hub = new FakeHub(); const api = new FakeHubApi(hub);
  const id = seedChat(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.prose pre code')).toBeVisible();
  for (const variant of VARIANTS) {
    await setVariant(page, variant.palette, variant.theme);
    await expect(page.locator('.prose pre code')).toBeVisible({ timeout: 15_000 });
    await save(page, 'chat-code', variant.key);
    await page.locator('.tool .head').last().click();
    await expect(page.locator('.tool .out pre')).toBeVisible();
    await save(page, 'tool-output', variant.key);
  }
});

test('permission_card_for_every_variant', async ({ page }) => {
  const hub = new FakeHub(); const api = new FakeHubApi(hub);
  const id = seedChat(api);
  await installChatRoutes(page, hub, api, OWNER);
  hub.append(`conversation:${id}`, {
    kind: 'request_opened', conversation_id: id,
    payload: { request_id: api.id(), kind: 'permission', payload: { tool_name: 'Bash', description: 'Запустить проверки', command: 'npm run check && npm run lint' } }
  });
  hub.flush();
  await page.goto(`/c/${id}`);
  await expect(page.locator('.permission-card')).toBeVisible({ timeout: 15_000 });
  for (const variant of VARIANTS) {
    await setVariant(page, variant.palette, variant.theme);
    await expect(page.locator('.permission-card')).toBeVisible({ timeout: 15_000 });
    await save(page, 'permission', variant.key);
  }
});

test('settings_for_every_variant', async ({ page }) => {
  const hub = new FakeHub(); const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await seedSecondaryScreens(page, api, ids);
  await page.goto('/settings');
  await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  for (const variant of VARIANTS) {
    await setVariant(page, variant.palette, variant.theme);
    await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
    await save(page, 'settings', variant.key);
  }
});
