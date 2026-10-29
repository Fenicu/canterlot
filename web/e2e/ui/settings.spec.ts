import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

test('settings_screen_shows_login_connection_theme_enter_install_and_signout', async ({ page, browserName }) => {
  await bootSecondary(page, '/settings');
  const main = page.locator('main');
  await expect(main.getByText('owner', { exact: true })).toBeVisible();
  await expect(main.getByText(/Последняя синхронизация/)).toBeVisible();
  await expect(main.getByText('Подключено', { exact: true })).toBeVisible();
  await main.getByRole('radio', { name: 'Светлая', exact: true }).check();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await main.getByRole('radio', { name: 'Выкл', exact: true }).check();
  await expect(page.locator('html')).toHaveAttribute('data-motion', 'off');
  await page.reload();
  await expect(page.locator('main').getByRole('radio', { name: 'Выкл', exact: true })).toBeChecked();
  await expect(main.getByRole('radiogroup', { name: 'Enter на клавиатуре' })).toBeVisible();
  if (browserName === 'chromium') {
    await page.evaluate(() => { const event = new Event('beforeinstallprompt'); Object.assign(event, { prompt: async () => {}, userChoice: Promise.resolve({ outcome: 'accepted' }) }); window.dispatchEvent(event); });
    await expect(main.getByRole('button', { name: 'Установить приложение' })).toBeVisible();
    await main.getByRole('button', { name: 'Установить приложение' }).click();
    await expect(main.getByRole('button', { name: 'Установить приложение' })).toHaveCount(0);
  }
  await expect(main.getByRole('button', { name: 'Выйти', exact: true })).toBeVisible();
  await expect(main.getByText(/Authelia.*собственной странице/)).toBeVisible();
});

test('enter_preference_changes_composer_behaviour', async ({ page }) => {
  const { ids } = await bootSecondary(page, '/settings');
  await page.locator('main').getByRole('radio', { name: 'Новая строка', exact: true }).check();
  await page.goto(`/c/${ids.pinned}`);
  const input = page.getByRole('textbox', { name: 'Сообщение', exact: true });
  await input.fill('Первая'); await input.press('Enter');
  await expect(input).toHaveValue('Первая\n');
  await page.goto('/settings');
  await page.locator('main').getByRole('radio', { name: 'Отправка', exact: true }).check();
  await page.goto(`/c/${ids.pinned}`);
  await input.fill('Отправить');
  const sent = page.waitForRequest(r => r.method() === 'POST' && r.url().endsWith('/inputs'));
  await input.press('Enter'); await sent;
  await expect(input).toHaveValue('');
});

test('sign_out_purges_local_data_and_closes_the_socket', async ({ page }) => {
  const { hub, ids } = await bootSecondary(page, '/settings');
  await page.goto(`/c/${ids.pinned}`);
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).fill('Приватный черновик');
  await page.goto('/settings');
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  await page.locator('main').getByRole('button', { name: 'Выйти', exact: true }).click();
  await expect(page.getByText('Данные на устройстве удалены', { exact: true })).toBeVisible();
  await expect.poll(() => hub.activeConnections === 0).toBe(true);
  expect(await page.evaluate(async () => {
    const db = await new Promise<IDBDatabase>(resolve => { const r = indexedDB.open('canterlot', 1); r.onsuccess = () => resolve(r.result); });
    const counts = await Promise.all(['drafts', 'ops', 'uploads', 'share_inbox'].map(store => new Promise<number>(resolve => { const r = db.transaction(store).objectStore(store).count(); r.onsuccess = () => resolve(r.result); })));
    db.close(); return { counts, preferences: Object.keys(localStorage).filter(k => k.startsWith('canterlot.')) };
  })).toEqual({ counts: [0, 0, 0, 0], preferences: [] });
});

test('sign_out_navigates_to_the_logout_url_from_config', async ({ page }) => {
  await page.route('**/logout-fixture', r => r.fulfill({ contentType: 'text/html', body: '<html lang="ru"><title>Выход</title><p>Выход завершён</p></html>' }));
  await bootSecondary(page, '/settings', '/logout-fixture');
  await page.locator('main').getByRole('button', { name: 'Выйти', exact: true }).click();
  await expect(page).toHaveURL(/\/logout-fixture$/);
});
