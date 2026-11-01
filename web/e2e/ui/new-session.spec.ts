import type { Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { FakeHub, FakeHubApi, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

async function boot(page: Page, options: { solo?: boolean; home?: string | null; models?: { id: string; title: string; efforts: string[] }[] } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const { machine } = seedMockupSessions(api);
  if (options.solo) {
    for (const id of [...api.machines.keys()]) if (id !== machine) api.machines.delete(id);
  }
  const record = api.machines.get(machine)!;
  if (options.home !== undefined) record.home_dir = options.home;
  if (options.models) record.harnesses[0]!.models = options.models;
  seedPresets(api);
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { api, hub, machine };
}

const pathField = (page: Page) => page.getByRole('textbox', { name: 'Рабочая папка' });
const hint = (page: Page, name: string) => page.getByRole('option', { name, exact: true });

async function openWithMachine(page: Page) {
  await page.goto('/new');
  await expect(page.getByRole('combobox', { name: 'Машина' })).not.toHaveValue('');
}

test('new_session_sheet_follows_mockup_07_and_creates_a_started_session', async ({ page }) => {
  await boot(page);
  const bodies: { start?: boolean; instruction_profile?: string; key: string }[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/v1/conversations') {
      bodies.push({ ...(request.postDataJSON() as { start?: boolean }), key: request.headers()['idempotency-key'] ?? '' });
    }
  });
  await page.getByRole('link', { name: 'Новая сессия' }).click();
  await expect(page.getByRole('option', { name: /Машина недоступна: связь нестабильна/ })).toHaveAttribute('disabled', '');
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

// QUARANTINE: ответ подсказок папки может прийти между нажатием и отпусканием и сдвинуть форму; 1 из 952 под нагрузкой.
test.describe('quarantine new_session_defaults_to_bypass_and_lets_the_owner_pick_a_labelled_mode', () => {
  test.describe.configure({ retries: 2 });
  test('new_session_defaults_to_bypass_and_lets_the_owner_pick_a_labelled_mode', async ({ page }) => {
    await boot(page);
    const bodies: { permission_mode?: string }[] = [];
    page.on('request', (request) => {
      if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/v1/conversations') {
        bodies.push({ ...(request.postDataJSON() as { permission_mode?: string }) });
      }
    });
    await page.goto('/new');
    await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
    await page.getByRole('textbox', { name: 'Рабочая папка' }).fill('/srv/canterlot');
    await page.getByRole('radio', { name: 'Claude Code' }).check();
    await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
    await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
    const mode = page.getByRole('combobox', { name: 'Режим разрешений' });
    await expect(mode).toHaveValue('bypass');
    const options = (await mode.locator('option').allTextContents()).map((text) => text.trim());
    expect(options).toEqual(['Как в настройках CLI', 'Спрашивать', 'Без подтверждений', 'Только чтение']);
    await page.getByRole('button', { name: 'Создать' }).click();
    await expect(page).toHaveURL(/\/c\//);
    expect(bodies[0]?.permission_mode).toBe('bypass');
  });
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

test('new_session_defaults_to_the_1m_variant_with_plain_ones_after_it', async ({ page }) => {
  await boot(page, {
    solo: true,
    models: [
      { id: 'opus[1m]', title: 'Opus 1M', efforts: ['medium'] },
      { id: 'fable[1m]', title: 'Fable 1M', efforts: ['medium'] },
      { id: 'opus', title: 'Opus', efforts: ['medium'] },
      { id: 'haiku', title: 'Haiku', efforts: ['medium'] }
    ]
  });
  await page.goto('/new');
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  const field = page.getByRole('combobox', { name: 'Модель' });
  await expect(field).toHaveValue('opus[1m]');
  await expect(field.locator('option')).toContainText(['Выберите модель', 'Opus 1M', 'Fable 1M', 'Opus', 'Haiku']);
  await field.selectOption('haiku');
  await expect(field).toHaveValue('haiku');
  await field.selectOption('opus[1m]');
  await pathField(page).fill('/srv/canterlot');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page).toHaveURL(/\/c\//);
  await expect(page.getByText(/Claude Code · Opus 1M · workstation/).first()).toBeVisible();
});

test('new_session_suggests_existing_folders_by_keyboard_and_continues_into_the_choice', async ({ page }) => {
  await boot(page, { solo: true });
  await openWithMachine(page);
  await pathField(page).fill('/srv/c');
  await expect(hint(page, '/srv/canterlot')).toBeVisible();
  await expect(page.getByRole('option', { name: '/srv/notes' })).toHaveCount(0);
  await pathField(page).press('ArrowDown');
  await expect(hint(page, '/srv/canterlot')).toHaveAttribute('aria-selected', 'true');
  await pathField(page).press('Enter');
  await expect(pathField(page)).toHaveValue('/srv/canterlot/');
  await expect(pathField(page)).toBeFocused();
  await expect(hint(page, '/srv/canterlot/web')).toBeVisible();
  await expect(page.getByRole('option', { name: '/srv/canterlot/.git' })).toHaveCount(0);
  await pathField(page).press('Escape');
  await expect(page.getByRole('option', { name: '/srv/canterlot/web' })).toHaveCount(0);
  await expect(pathField(page)).toBeVisible();
});

test('new_session_suggestion_is_chosen_by_pointer_and_the_field_keeps_focus', async ({ page, hasTouch }) => {
  await boot(page, { solo: true });
  await openWithMachine(page);
  await pathField(page).click();
  await pathField(page).pressSequentially('/srv/');
  if (hasTouch) await hint(page, '/srv/notes').tap();
  else await hint(page, '/srv/notes').click();
  await expect(pathField(page)).toHaveValue('/srv/notes/');
  await expect(pathField(page)).toBeFocused();
});

test('new_session_choices_by_pointer_are_not_lost_while_the_folder_list_is_open', async ({ page }) => {
  await boot(page, { solo: true });
  await openWithMachine(page);
  const list = page.getByRole('listbox', { name: 'Подсказки папок' });
  await pathField(page).fill('/srv/c');
  await expect(list).toBeVisible();
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await expect(page.getByRole('radio', { name: 'Claude Code' })).toBeChecked();
  await expect(list).toBeHidden();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await pathField(page).fill('/srv/canterlot');
  await expect(list).toBeVisible();
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page).toHaveURL(/\/c\//);
});

test('new_session_choices_survive_a_machine_list_refresh', async ({ page }) => {
  const { hub, machine } = await boot(page, { solo: true });
  const bodies: { harness?: string; model?: string; effort?: string; permission_mode?: string }[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/v1/conversations') bodies.push({ ...(request.postDataJSON() as object) });
  });
  await openWithMachine(page);
  await pathField(page).fill('/srv/canterlot');
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await page.getByRole('combobox', { name: 'Режим разрешений' }).selectOption('ask');
  const radio = page.getByRole('radio', { name: 'Claude Code' });
  const handle = await radio.elementHandle();
  const metrics = { machine_time_ms: Date.now(), cpu_cores: 8, cpu_permille: 500, mem_total_bytes: 1000, mem_available_bytes: 500, load1_centi: 100, load5_centi: 100, load15_centi: 100, disks: [], executions: [] };
  for (let tick = 0; tick < 3; tick += 1) {
    hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: machine, ...metrics, cpu_permille: 500 + tick } });
    hub.append('machines', { kind: 'machine_availability_changed', payload: { machine_id: machine, availability: 'available' } });
    hub.flush();
  }
  await expect(page.getByRole('combobox', { name: 'Машина' })).toHaveValue(machine);
  await expect(radio).toBeChecked();
  await expect(page.getByRole('combobox', { name: 'Модель' })).toHaveValue('opus');
  await expect(page.getByRole('combobox', { name: 'Усилие' })).toHaveValue('medium');
  await expect(page.getByRole('combobox', { name: 'Режим разрешений' })).toHaveValue('ask');
  await expect(pathField(page)).toHaveValue('/srv/canterlot');
  expect(await handle!.evaluate((node) => node.isConnected)).toBe(true);
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page).toHaveURL(/\/c\//);
  expect(bodies[0]).toMatchObject({ harness: 'claude', model: 'opus', effort: 'medium', permission_mode: 'ask' });
});

test('new_session_shows_no_list_and_no_error_when_the_machine_does_not_answer', async ({ page }) => {
  await boot(page, { solo: true });
  await page.route('**/api/v1/machines/*/workdirs/suggest?*', (route) => route.abort());
  await openWithMachine(page);
  await pathField(page).fill('/tmp/typed-by-hand');
  await page.waitForTimeout(400);
  await expect(page.getByRole('listbox', { name: 'Подсказки папок' })).toHaveCount(0);
  await expect(page.getByRole('alert')).toHaveCount(0);
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await expect(page.getByRole('button', { name: 'Создать' })).toBeEnabled();
});

test('new_session_ignores_a_late_answer_for_an_older_input', async ({ page }) => {
  const { api } = await boot(page, { solo: true });
  let older: (() => Promise<void>) | undefined;
  const held = new Promise<void>((resolve) => {
    void page.route('**/api/v1/machines/*/workdirs/suggest?*', async (route) => {
      const url = new URL(route.request().url());
      if (url.searchParams.get('path') === '/srv/c') {
        older = () => route.fulfill({ json: { workdirs: ['/srv/late-answer'] } }).catch(() => undefined);
        resolve();
        return;
      }
      await route.fulfill({ json: api.suggestWorkdirs(url.searchParams.get('path') ?? '') });
    });
  });
  await openWithMachine(page);
  await pathField(page).fill('/srv/c');
  await held;
  await pathField(page).fill('/srv/n');
  await expect(hint(page, '/srv/notes')).toBeVisible();
  await older?.();
  await page.waitForTimeout(100);
  await expect(hint(page, '/srv/notes')).toBeVisible();
  await expect(page.getByRole('option', { name: '/srv/late-answer' })).toHaveCount(0);
});

test('new_session_with_suggestions_has_no_axe_violations', async ({ page }) => {
  await boot(page, { solo: true });
  await openWithMachine(page);
  await pathField(page).fill('/srv/');
  await expect(hint(page, '/srv/notes')).toBeVisible();
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);
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
