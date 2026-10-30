import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

async function boot(page: Page, options: { solo?: boolean; home?: string | null } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const { machine } = seedMockupSessions(api);
  if (options.solo) {
    for (const id of [...api.machines.keys()]) if (id !== machine) api.machines.delete(id);
  }
  const record = api.machines.get(machine)!;
  if (options.home !== undefined) record.home_dir = options.home;
  seedPresets(api);
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { api, machine };
}

const pathField = (page: Page) => page.getByRole('textbox', { name: 'Рабочая папка' });
test('new_session_sheet_follows_mockup_07_and_creates_a_started_session', async ({ page }) => {
  await boot(page);
  const bodies: { start?: boolean; instruction_profile?: string; key: string }[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/v1/conversations') {
      bodies.push({ ...(request.postDataJSON() as { start?: boolean }), key: request.headers()['idempotency-key'] ?? '' });
    }
  });
  await page.getByRole('link', { name: 'Новая сессия' }).click();
  await expect(page.getByRole('option', { name: /Машина недоступна: degraded/ })).toHaveAttribute('disabled', '');
  await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await page.getByRole('textbox', { name: 'Рабочая папка' }).fill('/srv/canterlot');
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page).toHaveURL(/\/c\//);
  expect(bodies[0]?.start).toBe(true);
  expect(bodies[0]?.instruction_profile).toBe('clean');
  expect(bodies[0]?.key).not.toBe('');
});

test('new_session_offers_recent_workdirs_and_submits_a_typed_path', async ({ page }) => {
  await boot(page);
  const bodies: { workdir?: string }[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/v1/conversations') {
      bodies.push({ ...(request.postDataJSON() as { workdir?: string }) });
    }
  });
  await page.goto('/new');
  await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await expect(page.getByRole('button', { name: '/srv/canterlot' })).toBeVisible();
  await expect(page.getByRole('button', { name: '/srv/notes' })).toBeVisible();
  await page.getByRole('button', { name: '/srv/canterlot' }).click();
  await expect(page.getByRole('textbox', { name: 'Рабочая папка' })).toHaveValue('/srv/canterlot');
  await page.getByRole('textbox', { name: 'Рабочая папка' }).fill('/tmp/typed-by-hand');
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page).toHaveURL(/\/c\//);
  expect(bodies[0]?.workdir).toBe('/tmp/typed-by-hand');
});

test('new_session_takes_the_only_machine_and_loads_it_like_a_manual_pick', async ({ page }) => {
  const { machine } = await boot(page, { solo: true, home: '/home/owner' });
  await page.getByRole('link', { name: 'Новая сессия' }).click();
  await expect(page.getByRole('combobox', { name: 'Машина' })).toHaveValue(machine);
  await expect(page.getByRole('button', { name: '/srv/canterlot' })).toBeVisible();
  await expect(pathField(page)).toHaveValue('/home/owner');
  await expect(page.getByRole('radio', { name: 'Claude Code' })).toBeVisible();
});

test('new_session_takes_the_only_machine_when_it_arrives_after_the_form_opened', async ({ page }) => {
  const { machine } = await boot(page, { solo: true, home: '/home/owner' });
  await page.goto('/new');
  await expect(page.getByRole('combobox', { name: 'Машина' })).toHaveValue(machine);
  await expect(page.getByRole('button', { name: '/srv/canterlot' })).toBeVisible();
  await expect(pathField(page)).toHaveValue('/home/owner');
});

test('new_session_leaves_the_choice_to_the_owner_with_two_machines', async ({ page }) => {
  await boot(page, { home: '/home/owner' });
  await page.goto('/new');
  await expect(page.getByRole('option', { name: 'workstation' })).toBeAttached();
  await expect(page.getByRole('combobox', { name: 'Машина' })).toHaveValue('');
  await expect(pathField(page)).toHaveValue('');
});

test('new_session_prefills_home_and_recent_folders_do_not_replace_it', async ({ page }) => {
  await boot(page, { home: '/home/owner' });
  await page.goto('/new');
  await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await expect(page.getByRole('button', { name: '/srv/notes' })).toBeVisible();
  await expect(pathField(page)).toHaveValue('/home/owner');
  await page.getByRole('button', { name: '/srv/notes' }).click();
  await expect(pathField(page)).toHaveValue('/srv/notes');
});

test('new_session_stays_empty_for_a_worker_without_a_home', async ({ page }) => {
  await boot(page, { solo: true, home: null });
  await page.goto('/new');
  await expect(page.getByRole('button', { name: '/srv/notes' })).toBeVisible();
  await expect(pathField(page)).toHaveValue('');
});

test('new_session_crunch_default_text_comes_from_config', async ({ page }) => {
  await boot(page);
  await page.goto('/new');
  await page.getByRole('switch', { name: 'Кранч' }).click();
  await expect(page.getByRole('textbox', { name: 'Текст продолжения' })).toHaveValue('продолжи с места остановки');
});

test('new_session_defaults_to_clean_and_offers_custom_presets', async ({ page }) => {
  await boot(page);
  await page.goto('/new');
  const field = page.getByRole('combobox', { name: 'Инструкции' });
  await expect(field).toBeVisible();
  await expect(field).toHaveValue('clean');
  await expect(field.locator('option')).toContainText(['Без инструкций', 'Как у владельца', 'Исполнитель (английский)']);
  await field.selectOption('seed-executor');
  await expect(page.getByText('Общение, рассуждения и отчёты на английском').first()).toBeVisible();
});

test('list_and_new_session_have_no_horizontal_scroll_at_360px_and_320px_and_200_percent_text', async ({ page }) => {
  await boot(page);
  for (const width of [360, 320]) {
    await page.setViewportSize({ width, height: 640 });
    await page.evaluate(() => {
      document.documentElement.style.fontSize = '32px';
    });
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(1);
    await page.goto('/new');
    const again = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(again).toBeLessThanOrEqual(1);
    await page.goto('/sessions');
  }
});
