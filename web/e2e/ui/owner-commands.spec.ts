import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, fillWorkdir, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  seedPresets(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { api, ids };
}

/** Два нажатия в одной задаче: интерфейс не успевает перерисоваться между ними. */
async function doubleTap(page: Page, name: string) {
  await page.getByRole('button', { name, exact: true }).evaluate((button: HTMLButtonElement) => {
    button.click();
    button.click();
  });
}

const proxyError = { status: 502, contentType: 'text/html', body: '<html><body><h1>502 Bad Gateway</h1></body></html>' };

test('double_tap_on_create_makes_one_session', async ({ page }) => {
  await boot(page);
  const keys: string[] = [];
  let release!: () => void;
  const held = new Promise<void>((done) => (release = done));
  await page.route('**/api/v1/conversations', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    keys.push(route.request().headers()['idempotency-key'] ?? '');
    await held;
    await route.fallback();
  });
  await page.goto('/new');
  await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await fillWorkdir(page, '/srv/canterlot');
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  await doubleTap(page, 'Создать');
  await expect(page.getByRole('button', { name: 'Создаю…' })).toBeDisabled();
  release();
  await expect(page).toHaveURL(/\/c\//, { timeout: 15_000 });
  expect(keys).toHaveLength(1);
});

test('group_create_shows_proxy_outage_and_a_double_tap_sends_once', async ({ page }) => {
  await boot(page);
  const keys: string[] = [];
  let failNext = true;
  await page.route('**/api/v1/groups', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    keys.push(route.request().headers()['idempotency-key'] ?? '');
    if (failNext) {
      failNext = false;
      return route.fulfill(proxyError);
    }
    await route.fallback();
  });
  await page.getByRole('button', { name: 'Ещё' }).click();
  await page.getByRole('menuitem', { name: 'Группы' }).click();
  await page.getByRole('textbox', { name: 'Название группы' }).fill('черновики');
  await page.getByRole('button', { name: 'Создать', exact: true }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'Хаб недоступен, повторяю' })).toBeVisible();
  await expect(page.getByRole('dialog', { name: 'Сессия истекла' })).toBeHidden();
  await doubleTap(page, 'Создать');
  await expect(page.getByRole('tab', { name: /черновики/ })).toBeVisible();
  expect(keys).toHaveLength(2);
  // Повтор после обрыва идёт тем же ключом: хаб не заведёт вторую группу.
  expect(keys[1]).toBe(keys[0]);
});

test('failed_pin_is_reported_and_rolled_back', async ({ page }) => {
  const { ids } = await boot(page);
  await page.route(`**/api/v1/conversations/${ids.plain}`, async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    await route.fulfill({
      status: 409,
      contentType: 'application/json',
      body: JSON.stringify({
        error: { code: 'invalid_state', message: 'Действие недопустимо в текущем состоянии', scope: 'delivery', retry: 'never', correlation_id: 'c-pin' }
      })
    });
  });
  await page.getByRole('button', { name: 'Действия: Повторная доставка ввода' }).click();
  const pin = page.getByRole('switch', { name: 'Закрепить' });
  const before = await pin.isChecked();
  await pin.click();
  await expect(page.getByRole('alert').filter({ hasText: 'Действие недопустимо в текущем состоянии' })).toBeVisible();
  await expect(pin).toBeChecked({ checked: before });
});
