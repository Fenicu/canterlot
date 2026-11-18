import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d4';
const MACHINE = '00000000-0000-4000-8000-0000000000b1';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const CATALOG = [
  { name: 'review', description: 'Review the current changes', kind: 'command', argument_hint: '[file]' },
  { name: 'preview', description: 'Show a preview', kind: 'command', argument_hint: '' },
  { name: 'brainstorming', description: 'Think before building', kind: 'skill', argument_hint: '' }
];

interface Boot {
  catalog?: typeof CATALOG | null;
  harness?: typeof CLAUDE;
}

async function boot(page: Page, opts: Boot = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const harness = opts.harness ?? CLAUDE;
  seedMachine(api, {
    id: MACHINE,
    harnesses: [
      {
        id: 'claude_code',
        title: 'Claude Code',
        provider: 'claude',
        models: [{ id: 'opus', title: 'Opus', efforts: [] }],
        permission_modes: ['owner'],
        ...(opts.catalog === null ? {} : { catalog: opts.catalog ?? CATALOG })
      }
    ]
  });
  const id = seedConversation(api, { machine: { id: MACHINE, name: 'dev' }, harness });
  const posts: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && /\/api\/v1\/conversations\/[^/]+\/inputs$/.test(request.url())) {
      posts.push(request.url());
    }
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(area).toBeVisible();
  return { hub, api, id, area, posts };
}

test('slash_at_the_start_opens_a_filtered_catalog_with_descriptions', async ({ page }) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.click();
  await area.press('/');
  const list = page.getByRole('listbox', { name: 'Команды и навыки' });
  await expect(list).toBeVisible();
  await expect(list.getByRole('option')).toHaveCount(3);
  await expect(list.getByText('Review the current changes')).toBeVisible();
  await expect(list.getByText('[file]')).toBeVisible();
  await expect(list.getByRole('option', { name: /brainstorming/ }).getByText('навык')).toBeVisible();

  await area.pressSequentially('rev');
  const names = await list.getByRole('option').allTextContents();
  expect(names.map((text) => text.match(/\/[a-z-]+/)?.[0])).toEqual(['/review', '/preview']);
  await expect(list.getByText('Think before building')).toHaveCount(0);
});

test('typing_a_space_or_deleting_the_slash_closes_the_catalog', async ({ page }) => {
  await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.click();
  await area.press('/');
  const list = page.getByRole('listbox', { name: 'Команды и навыки' });
  await expect(list).toBeVisible();
  await area.pressSequentially('review ');
  await expect(list).toHaveCount(0);
  for (let i = 0; i < 8; i++) await area.press('Backspace');
  await expect(area).toHaveValue('');
  await expect(list).toHaveCount(0);
});

test('keyboard_arrows_enter_and_escape_choose_without_sending', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'клавиатура компьютера');
  const { posts } = await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.focus();
  await area.press('/');
  const list = page.getByRole('listbox', { name: 'Команды и навыки' });
  await expect(list).toBeVisible();
  await area.press('ArrowDown');
  await expect(list.getByRole('option').nth(1)).toHaveAttribute('aria-selected', 'true');
  await area.press('Enter');
  await expect(area).toHaveValue('/preview ');
  await expect(area).toBeFocused();
  await expect(list).toHaveCount(0);
  expect(posts).toEqual([]);

  await area.fill('/');
  await expect(list).toBeVisible();
  await area.press('Escape');
  await expect(list).toHaveCount(0);
  await expect(area).toHaveValue('/');
  expect(posts).toEqual([]);
});

test('touch_tap_inserts_the_command_and_sends_nothing', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'нажатие пальцем');
  const { posts, api, id } = await boot(page);
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await area.click();
  await area.press('/');
  const list = page.getByRole('listbox', { name: 'Команды и навыки' });
  await expect(list).toBeVisible();
  await list.getByRole('option', { name: /\/review/ }).tap();
  await expect(area).toHaveValue('/review ');
  await expect(list).toHaveCount(0);
  expect(posts).toEqual([]);
  await expect.poll(() => api.draft(id).text).toBe('/review ');
});

test('no_catalog_for_another_harness_or_when_the_worker_has_none', async ({ page }) => {
  const other = await boot(page, { harness: { id: 'codex', title: 'Codex', provider: 'codex' } });
  await other.area.click();
  await other.area.press('/');
  await expect(page.getByRole('listbox', { name: 'Команды и навыки' })).toHaveCount(0);
  await page.goto('/');
  const empty = await boot(page, { catalog: null });
  await empty.area.click();
  await empty.area.press('/');
  await expect(page.getByRole('listbox', { name: 'Команды и навыки' })).toHaveCount(0);
});
