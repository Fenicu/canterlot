import type { Page } from '@playwright/test';
import { test, expect } from './fixtures';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import type { FakeAuth } from '../../testing/fake-hub/auth';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';

const QR_SECRET = 'S'.repeat(43);
const RECOVERY = 'R'.repeat(43);

async function boot(page: Page, path: string, setup?: (auth: FakeAuth) => void) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  setup?.(hub.auth);
  await installChatRoutes(page, hub, api, ids.pinned);
  await seedSecondaryScreens(page, api, ids);
  await page.goto(path);
  await expect(page.locator('[data-layout]')).toBeVisible();
  return { hub, api, ids, auth: hub.auth };
}

const signIn = (page: Page) => page.getByRole('heading', { name: 'Вход в canterlot' });

/** Ключ доступа без настоящего аутентификатора: браузер возвращает готовый ответ. */
async function fakeAuthenticator(page: Page) {
  await page.addInitScript(() => {
    const answer = { toJSON: () => ({ id: 'AQID', rawId: 'AQID', type: 'public-key', response: { clientDataJSON: 'e30', attestationObject: 'oA' }, clientExtensionResults: {} }) };
    Object.defineProperty(navigator.credentials, 'create', { value: async () => answer });
    Object.defineProperty(navigator.credentials, 'get', { value: async () => answer });
  });
}

test('sign_in_page_takes_a_password_and_opens_the_app', async ({ page }) => {
  const { auth } = await boot(page, '/sessions', (a) => { a.signedIn = false; });
  await expect(signIn(page)).toBeVisible();
  const login = page.getByRole('textbox', { name: 'Логин' });
  const password = page.getByLabel('Пароль', { exact: true });
  await expect(login).toHaveAttribute('autocomplete', 'username');
  await expect(password).toHaveAttribute('autocomplete', 'current-password');
  await expect(page.locator('form').filter({ has: password })).toHaveCount(1);
  await expect(page.getByRole('button', { name: 'Войти с ключом доступа' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Войти по QR' })).toBeVisible();
  await login.fill('owner');
  await password.fill('не тот пароль');
  await page.getByRole('button', { name: 'Войти', exact: true }).click();
  await expect(page.getByRole('alert')).toHaveText('Неверный логин или пароль');
  await password.fill(auth.password!);
  await password.press('Enter');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(signIn(page)).toHaveCount(0);
  const call = auth.calls.find((item) => item.path === '/password' && (item.body as { password?: string }).password === auth.password);
  expect((call?.body as { device?: { standalone?: boolean } }).device?.standalone).toBe(false);
});

test('sign_in_page_names_the_lock_and_hides_passkeys_without_them', async ({ page }, testInfo) => {
  await boot(page, '/sessions', (a) => { a.signedIn = false; a.lockSeconds = 240; a.passkeys = []; });
  await expect(signIn(page)).toBeVisible();
  await expect(page.getByRole('button', { name: 'Войти по QR' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Войти с ключом доступа' })).toHaveCount(0);
  await page.getByRole('textbox', { name: 'Логин' }).fill('owner');
  await page.getByLabel('Пароль', { exact: true }).fill('что угодно');
  await page.getByRole('button', { name: 'Войти', exact: true }).click();
  await expect(page.getByRole('alert')).toHaveText('Слишком много попыток. Повторите через 4 мин');
  await page.getByRole('alert').evaluate((node) => node.remove());
  await page.screenshot({ path: `/tmp/login-signin-${testInfo.project.name}.png` });
});

test('hub_unavailable_is_not_the_sign_in_page', async ({ page }) => {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  hub.routes.set('GET /api/v1/me', (route) => route.fulfill({ status: 503, json: { error: { code: 'db_unavailable', message: 'База недоступна', scope: 'hub_db', retry: 'safe_same_key', correlation_id: '00000000-0000-4000-8000-0000000000b1' } } }));
  await page.goto('/sessions');
  await expect(page.locator('[data-layout]')).toBeVisible();
  await page.waitForTimeout(1500);
  await expect(signIn(page)).toHaveCount(0);
});

test('qr_panel_shows_the_code_scanned_state_restarts_expired_codes_and_signs_in', async ({ page }) => {
  const { auth } = await boot(page, '/sessions', (a) => { a.signedIn = false; });
  await page.getByRole('button', { name: 'Войти по QR' }).click();
  const code = page.getByRole('img', { name: 'Код входа в canterlot' });
  await expect(code).toBeVisible();
  await expect(code).toHaveAttribute('data-qr', /\/qr#Q+1$/);
  await expect(page.getByRole('status').filter({ hasText: /Код обновится через \d+ с/ })).toBeVisible();
  await page.screenshot({ path: '/tmp/login-qr.png' });
  auth.qrScript.push('expired');
  await expect(code).toHaveAttribute('data-qr', /\/qr#Q+2$/);
  auth.qrScript.push('scanned');
  await expect(page.getByText('Код отсканирован, подтвердите на другом устройстве')).toBeVisible();
  const polls = auth.calls.filter((call) => call.path === '/qr/poll');
  await expect.poll(() => auth.calls.filter((call) => call.path === '/qr/poll').length).toBeGreaterThan(polls.length);
  expect((auth.calls.filter((call) => call.path === '/qr/poll').at(-1)!.body as { after: string }).after).toBe('scanned');
  auth.qrScript.push('approved');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
});

test('qr_panel_denied_offers_a_new_code_and_cancel_returns_to_the_form', async ({ page }) => {
  const { auth } = await boot(page, '/sessions', (a) => { a.signedIn = false; a.qrScript = ['denied']; });
  await page.getByRole('button', { name: 'Войти по QR' }).click();
  await expect(page.getByText('Вход отклонён на другом устройстве')).toBeVisible();
  await page.getByRole('button', { name: 'Показать новый код' }).click();
  await expect(page.getByRole('img', { name: 'Код входа в canterlot' })).toBeVisible();
  expect(auth.qrStarts).toBe(2);
  await page.getByRole('button', { name: 'Отмена' }).click();
  await expect(page.getByRole('button', { name: 'Войти', exact: true })).toBeVisible();
});

test('sessions_screen_lists_sessions_with_place_ip_and_method', async ({ page }, testInfo) => {
  await boot(page, '/settings/sign-in');
  const main = page.locator('main');
  await expect(page.locator('main[data-secondary-ready]')).toBeVisible();
  const rows = main.locator('button[data-session]');
  await expect(rows).toHaveCount(4);
  await expect(rows.first()).toContainText('Chrome 142 · Android 16 · Pixel 9');
  await expect(rows.first()).toContainText('этот сеанс');
  await expect(rows.nth(1)).toContainText('Рабочий ноутбук');
  await expect(rows.nth(1)).toContainText('Firefox 159 · Linux · 192.168.1.20 · локальная сеть · был(а) в сети 5 мин назад');
  await expect(rows.nth(1)).toContainText('вход ключом доступа');
  await expect(rows.nth(2)).toContainText('IP неизвестен');
  await expect(rows.nth(3)).toContainText('Казань, Россия');
  await expect(main.getByRole('link', { name: 'IP-геолокация: DB-IP' })).toHaveAttribute('href', 'https://db-ip.com');
  await expect(main.getByRole('button', { name: 'Сканировать код' })).toBeVisible();
  await expect(main.getByRole('button', { name: 'Сменить пароль' })).toBeVisible();
  if (testInfo.project.name === 'phone') await page.screenshot({ path: '/tmp/login-sessions-phone.png', fullPage: true });
  await rows.nth(1).click();
  const sheet = page.getByRole('dialog', { name: 'Рабочий ноутбук' });
  await expect(sheet).toContainText('Mozilla/5.0 (X11; Linux x86_64; rv:159.0) Gecko/20100101 Firefox/159.0');
  await sheet.getByRole('button', { name: 'Переименовать' }).click();
  await sheet.getByRole('textbox', { name: 'Имя сеанса' }).fill('Домашний ноутбук');
  await sheet.getByRole('button', { name: 'Сохранить' }).click();
  await expect(rows.nth(1)).toContainText('Домашний ноутбук');
});

test('terminating_a_session_asks_to_confirm_the_sign_in_and_names_the_cascade', async ({ page }) => {
  const { auth } = await boot(page, '/settings/sign-in');
  const rows = page.locator('main button[data-session]');
  await expect(rows).toHaveCount(4);
  await rows.nth(2).click();
  const sheet = page.getByRole('dialog', { name: 'Safari 19 · iOS 19' });
  await expect(sheet).toContainText('Вход подтвердил');
  await sheet.getByRole('button', { name: 'Завершить сеанс' }).click();
  const confirm = page.getByRole('dialog', { name: 'Завершить сеанс?' });
  await expect(confirm).toContainText('Также будет завершён 1 сеанс, вход которого подтвердило это устройство: Планшет на кухне');
  await confirm.getByRole('button', { name: 'Завершить' }).click();
  const prompt = page.getByRole('dialog', { name: 'Подтвердите вход' });
  await expect(prompt).toBeVisible();
  await prompt.getByLabel('Пароль').fill('неверный');
  await prompt.getByRole('button', { name: 'Подтвердить', exact: true }).click();
  await expect(prompt.getByText('Неверный пароль')).toBeVisible();
  await expect(signIn(page)).toHaveCount(0);
  await prompt.getByLabel('Пароль').fill(auth.password!);
  await prompt.getByRole('button', { name: 'Подтвердить', exact: true }).click();
  await expect(prompt).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: 'Сеанс завершён' })).toBeVisible();
  await expect(rows).toHaveCount(2);
  expect(auth.calls.filter((call) => call.method === 'DELETE' && call.path.startsWith('/sessions/'))).toHaveLength(2);
});

test('terminate_others_names_the_sessions_this_device_approved_and_idle_timeout_saves', async ({ page }) => {
  const { auth } = await boot(page, '/settings/sign-in', (a) => { a.fresh = true; });
  const rows = page.locator('main button[data-session]');
  await expect(rows).toHaveCount(4);
  await page.getByRole('button', { name: 'Завершить все остальные сеансы' }).click();
  const confirm = page.getByRole('dialog', { name: 'Завершить все остальные сеансы?' });
  await expect(confirm).toContainText('Будут завершены все сеансы, кроме этого: 3.');
  await expect(confirm).toContainText('Также будут завершены 2 сеанса, вход которых подтвердило это устройство: Safari 19 · iOS 19, Планшет на кухне');
  await confirm.getByRole('button', { name: 'Завершить' }).click();
  await expect(rows).toHaveCount(1);
  await expect(page.getByRole('button', { name: 'Завершить все остальные сеансы' })).toHaveCount(0);
  const idle = page.getByRole('radiogroup', { name: 'Срок бездействия' });
  await expect(idle.getByRole('radio', { name: '3 месяца' })).toBeChecked();
  await expect(page.locator('[data-idle-note]')).toContainText('только получает события, сеанс не продлевает');
  await idle.getByText('1 год').click();
  await expect.poll(() => auth.idle).toBe(365);
});

test('passkeys_add_and_delete_ask_to_confirm_the_sign_in_and_rename_does_not', async ({ page }) => {
  await fakeAuthenticator(page);
  const { auth } = await boot(page, '/settings/sign-in');
  const keys = page.locator('main [data-passkey]');
  await expect(keys).toHaveCount(1);
  await page.getByRole('button', { name: 'Добавить ключ доступа' }).click();
  const prompt = page.getByRole('dialog', { name: 'Подтвердите вход' });
  await prompt.getByRole('button', { name: 'Подтвердить ключом доступа' }).click();
  await expect(prompt).toBeHidden();
  await expect(keys).toHaveCount(2);
  expect(auth.calls.filter((call) => call.path === '/passkeys/register/start')).toHaveLength(2);
  await page.getByRole('button', { name: 'Переименовать ключ Bitwarden' }).click();
  await page.getByRole('dialog', { name: 'Имя ключа доступа' }).getByRole('textbox', { name: 'Имя ключа' }).fill('Bitwarden дома');
  await page.getByRole('dialog', { name: 'Имя ключа доступа' }).getByRole('button', { name: 'Сохранить' }).click();
  await expect(keys.first()).toContainText('Bitwarden дома');
  auth.fresh = false;
  await page.getByRole('button', { name: 'Удалить ключ Bitwarden дома' }).click();
  await page.getByRole('dialog', { name: 'Удалить ключ доступа?' }).getByRole('button', { name: 'Удалить' }).click();
  await expect(prompt).toBeVisible();
  await prompt.getByLabel('Пароль').fill(auth.password!);
  await prompt.getByRole('button', { name: 'Подтвердить', exact: true }).click();
  await expect(keys).toHaveCount(1);
});

test('password_change_checks_the_current_password_and_setting_a_first_one_works', async ({ page }) => {
  const { auth } = await boot(page, '/settings/sign-in');
  await page.getByRole('button', { name: 'Сменить пароль' }).click();
  await page.getByLabel('Текущий пароль').fill('неверный');
  await page.getByLabel('Новый пароль').fill('новый-пароль-2026');
  await page.getByLabel('Повторите пароль').fill('новый-пароль-2026');
  await page.getByRole('button', { name: 'Сохранить пароль' }).click();
  await expect(page.getByText('Неверный пароль')).toBeVisible();
  await expect(signIn(page)).toHaveCount(0);
  await page.getByLabel('Текущий пароль').fill(auth.password!);
  await page.getByRole('button', { name: 'Сохранить пароль' }).click();
  await expect(page.getByText('Пароль изменён. Остальные сеансы завершены')).toBeVisible();
  expect(auth.password).toBe('новый-пароль-2026');

  auth.password = null; auth.fresh = true;
  await page.reload();
  await page.getByRole('button', { name: 'Задать пароль' }).click();
  await expect(page.getByLabel('Текущий пароль')).toHaveCount(0);
  await page.getByLabel('Новый пароль').fill('short-1');
  await page.getByLabel('Повторите пароль').fill('short-1');
  await page.getByRole('button', { name: 'Сохранить пароль' }).click();
  await expect(page.getByText('Пароль должен быть не короче 10 символов')).toBeVisible();
  await page.getByLabel('Новый пароль').fill('первый-пароль-2026');
  await page.getByLabel('Повторите пароль').fill('первый-пароль-2026');
  await page.getByRole('button', { name: 'Сохранить пароль' }).click();
  await expect(page.getByText('Пароль задан. Остальные сеансы завершены')).toBeVisible();
});

test('qr_link_signed_out_explains_where_to_scan_and_strips_the_fragment', async ({ page }) => {
  await boot(page, `/qr#${QR_SECRET}`, (a) => { a.signedIn = false; });
  await expect(page.getByText('Откройте этот код на устройстве, где вы уже вошли в canterlot, или отсканируйте его в приложении: Настройки → Сеансы и вход → Сканировать код')).toBeVisible();
  await expect.poll(() => new URL(page.url()).hash).toBe('');
  await expect(signIn(page)).toHaveCount(0);
});

test('qr_link_signed_in_shows_the_approve_card_and_approves', async ({ page }, testInfo) => {
  const { auth } = await boot(page, `/qr#${QR_SECRET}`, (a) => { a.inspect = { ...a.inspect, other_country: true }; });
  await expect.poll(() => new URL(page.url()).hash).toBe('');
  const card = page.locator('[data-qr-approve]');
  await expect(card.getByText('код показан 12 с назад')).toBeVisible();
  await expect(card.getByText('203.0.113.9')).toBeVisible();
  await expect(card.getByText('Казань, Россия')).toBeVisible();
  await expect(card.getByText('заявлено устройством: Firefox 159 · Linux')).toBeVisible();
  await expect(card.getByText('Запрашивает вход в canterlot. Разрешайте, только если этот код сейчас показан на вашем собственном экране.')).toBeVisible();
  await expect(card.getByRole('alert')).toContainText('из другой страны');
  if (testInfo.project.name === 'phone') await page.screenshot({ path: '/tmp/login-approve.png' });
  await card.getByRole('button', { name: 'Разрешить' }).click();
  await expect(card.getByText('Вход разрешён. Новое устройство войдёт через несколько секунд')).toBeVisible();
  const approve = auth.calls.find((call) => call.path === '/qr/approve');
  expect(approve?.body).toEqual({ secret: QR_SECRET });
  expect(await page.evaluate(() => JSON.stringify(localStorage) + JSON.stringify(sessionStorage) + JSON.stringify(history.state))).not.toContain(QR_SECRET);
});

test('recovery_link_signs_in_and_opens_sessions_with_the_banner', async ({ page }) => {
  const { auth } = await boot(page, `/recover#${RECOVERY}`, (a) => { a.signedIn = false; });
  await expect(page).toHaveURL(/\/settings\/sign-in\?from=recovery$/, { timeout: 15_000 });
  await expect(page.locator('[data-recovery-banner]')).toContainText('Вы вошли по ссылке восстановления');
  expect(auth.calls.find((call) => call.path === '/recovery')?.body).toMatchObject({ token: RECOVERY });
  expect(await page.evaluate(() => JSON.stringify(localStorage) + JSON.stringify(sessionStorage) + JSON.stringify(history.state))).not.toContain(RECOVERY);
  await page.locator('[data-recovery-banner]').getByRole('button', { name: 'Понятно' }).click();
  await expect(page.locator('[data-recovery-banner]')).toHaveCount(0);
  await expect(page).toHaveURL(/\/settings\/sign-in$/);
});

test('bad_recovery_link_says_so_in_words', async ({ page }) => {
  await boot(page, '/recover#not-the-token', (a) => { a.signedIn = false; });
  await expect(page.getByRole('alert')).toContainText('Ссылка восстановления недействительна');
  await expect.poll(() => new URL(page.url()).hash).toBe('');
});

test('revoked_session_wipes_local_data_and_shows_sign_in', async ({ page }) => {
  const { hub } = await boot(page, '/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await page.evaluate(() => localStorage.setItem('canterlot.theme', 'light'));
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  hub.auth.signedIn = false;
  hub.auth.signOut = { code: 'session_revoked', reason: 'owner' };
  hub.chaos.closeRevoked();
  await expect(signIn(page)).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText('Данные на этом устройстве удалены')).toBeVisible();
  expect(await page.evaluate(() => Object.keys(localStorage).filter((key) => key.startsWith('canterlot.')))).toEqual([]);
});

test('revoked_close_keeps_local_data_until_me_answers_401', async ({ page }) => {
  const { hub } = await boot(page, '/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await page.evaluate(() => localStorage.setItem('canterlot.theme', 'light'));
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  // Закрытие revoked, а /me не отвечает о сеансе: 503, затем обрыв сети.
  hub.auth.meFailure = 503;
  hub.chaos.closeRevoked();
  await page.waitForTimeout(1_500);
  await expect(page.getByText('Данные на этом устройстве удалены')).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem('canterlot.theme'))).toBe('light');
  hub.auth.meFailure = 'network';
  await page.waitForTimeout(2_500);
  await expect(page.getByText('Данные на этом устройстве удалены')).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem('canterlot.theme'))).toBe('light');
  // Хаб вернулся и подтвердил отзыв владельцем: только теперь данные стираются.
  hub.auth.meFailure = null;
  hub.auth.signedIn = false;
  hub.auth.signOut = { code: 'session_revoked', reason: 'owner' };
  await expect(signIn(page)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText('Данные на этом устройстве удалены')).toBeVisible();
  expect(await page.evaluate(() => Object.keys(localStorage).filter((key) => key.startsWith('canterlot.')))).toEqual([]);
});

test('limit_and_idle_endings_show_sign_in_but_keep_local_data', async ({ page }) => {
  const { hub } = await boot(page, '/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await page.evaluate(() => localStorage.setItem('canterlot.theme', 'light'));
  await expect.poll(() => hub.activeConnections).toBeGreaterThan(0);
  hub.auth.signedIn = false;
  hub.auth.signOut = { code: 'session_revoked', reason: 'limit' };
  hub.chaos.closeRevoked();
  await expect(signIn(page)).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText('Сеанс завершён: открыто слишком много сеансов. Войдите снова')).toBeVisible();
  await page.waitForTimeout(500);
  expect(await page.evaluate(() => localStorage.getItem('canterlot.theme'))).toBe('light');

  hub.auth.signOut = { code: 'session_expired' };
  await page.reload();
  await expect(signIn(page)).toBeVisible();
  await expect(page.getByText('Сеанс завершён из-за бездействия. Войдите снова')).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem('canterlot.theme'))).toBe('light');
});

test('scan_code_without_a_camera_says_so_in_words', async ({ page }) => {
  await boot(page, '/settings/sign-in');
  await page.getByRole('button', { name: 'Сканировать код' }).click();
  await expect(page.locator('[data-scanner]').getByRole('alert')).toContainText(/камер/i, { timeout: 10_000 });
  await page.locator('[data-scanner]').getByRole('button', { name: 'Отмена' }).click();
  await expect(page.locator('[data-scanner]')).toHaveCount(0);
});
