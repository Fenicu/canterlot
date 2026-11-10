import { test, expect, fillContrast } from './fixtures';
import { bootSecondary } from './secondary-fixture';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { PALETTES } from '../../src/lib/palettes';

test('settings_screen_shows_login_connection_theme_enter_install_and_signout', async ({ page, browserName }) => {
  await bootSecondary(page, '/settings');
  const main = page.locator('main');
  await expect(main.getByText('owner', { exact: true })).toBeVisible();
  await expect(main.getByText(/Последняя синхронизация/)).toBeVisible();
  await expect(main.getByText('Подключено', { exact: true })).toBeVisible();
  await main.getByRole('radio', { name: 'Светлый', exact: true }).check();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'standard');
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
  // Обе кнопки раздела во всю ширину, а подпись после двоеточия со строчной.
  const clear = (await main.getByRole('button', { name: 'Очистить данные на устройстве' }).boundingBox())!;
  const out = (await main.getByRole('button', { name: 'Выйти', exact: true }).boundingBox())!;
  expect(Math.abs(clear.width - out.width)).toBeLessThanOrEqual(1);
  await expect(main.getByText(/^Обновление: [а-яё]/)).toBeVisible();
  await expect(main.getByText(/Authelia.*собственной странице/)).toBeVisible();
});

test('chosen_palette_and_mode_stand_out_in_every_palette_and_mode', async ({ page }) => {
  // Тридцать смен темы: проверяется контраст, а не анимация смены, поэтому без перехода вида.
  test.setTimeout(60_000);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await bootSecondary(page, '/settings');
  const main = page.locator('main');
  const palettes = main.getByRole('radiogroup', { name: 'Тема' });
  const modes = main.getByRole('radiogroup', { name: 'Режим' });
  for (const { key, name } of PALETTES) {
    await palettes.getByRole('radio', { name, exact: true }).check();
    for (const mode of ['Тёмный', 'Светлый']) {
      await modes.getByRole('radio', { name: mode, exact: true }).check();
      // Отмеченная радиокнопка ещё не значит, что страница уже перекрашена.
      await expect(page.locator('html')).toHaveAttribute('data-palette', key);
      await expect(page.locator('html')).toHaveAttribute('data-theme', mode === 'Тёмный' ? 'dark' : 'light');
      // Кольцо и метка выбранной карточки отличимы от её фона и от фона страницы на 3:1.
      const ring = await palettes.locator('label:has(input:checked)').evaluate((node) => {
        const rgb = (value: string) => (value.match(/[\d.]+/g) ?? []).map(Number);
        const lum = (value: string) => { const [r, g, b] = rgb(value); return [r!, g!, b!].map((c) => { const s = c / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; }).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i]!, 0); };
        const ratio = (x: string, y: string) => { const [a, b] = [lum(x), lum(y)].sort((p, q) => q - p); return (a! + 0.05) / (b! + 0.05); };
        const card = getComputedStyle(node).backgroundColor;
        const page = getComputedStyle(document.body).backgroundColor;
        const outline = getComputedStyle(node).outlineColor;
        const mark = getComputedStyle(node.querySelector('.mark')!).backgroundColor;
        return Math.min(ratio(outline, card), ratio(outline, page), ratio(mark, card));
      });
      expect(ring, `${name} ${mode}`).toBeGreaterThanOrEqual(3);
      expect(await fillContrast(modes.locator('input:checked + span')), `${name} ${mode}`).toBeGreaterThanOrEqual(3);
    }
  }
});

test('palette_and_mode_apply_before_paint_and_survive_reload', async ({ page }) => {
  await bootSecondary(page, '/settings');
  const main = page.locator('main');
  await main.getByRole('radio', { name: 'Сепия', exact: true }).check();
  await main.getByRole('radio', { name: 'Светлый', exact: true }).check();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'sepia');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect.poll(() => page.evaluate(() => [localStorage.getItem('canterlot.palette'), localStorage.getItem('canterlot.theme')])).toEqual(['sepia', 'light']);

  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'sepia');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');

  // Без скриптов приложения работают только тема-инициализатор и CSS: атрибуты и фон уже верные.
  const probe = await page.context().newPage();
  await probe.route('**/_app/**', route => route.request().resourceType() === 'script' ? route.abort() : route.continue());
  await probe.goto('/settings');
  await expect(probe.locator('html')).toHaveAttribute('data-palette', 'sepia');
  await expect(probe.locator('html')).toHaveAttribute('data-theme', 'light');
  expect(await probe.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe('rgb(244, 236, 216)');
  await probe.close();
});

test('header_toggle_keeps_the_palette', async ({ page }) => {
  await bootSecondary(page, '/settings');
  const main = page.locator('main');
  await main.getByRole('radio', { name: 'Лес', exact: true }).check();
  await main.getByRole('radio', { name: 'Тёмный', exact: true }).check();
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Светлая тема' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'forest');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'forest');
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

test('clearing_data_in_one_window_reloads_the_other_and_keeps_only_the_shared_draft', async ({ page, context }) => {
  const { hub, api, ids } = await bootSecondary(page, '/settings');
  const other = await context.newPage();
  await installChatRoutes(other, hub, api, ids.pinned);
  await seedSecondaryScreens(other, api, ids, null);
  await other.goto(`/c/${ids.pinned}`);
  const input = other.getByRole('textbox', { name: 'Сообщение', exact: true });
  await input.fill('Черновик второго окна');
  await expect.poll(() => other.evaluate(() => Object.keys(localStorage).some(k => k.startsWith('canterlot.draft-tail.'))
    || new Promise<boolean>(resolve => { const r = indexedDB.open('canterlot', 1); r.onsuccess = () => { const q = r.result.transaction('drafts').objectStore('drafts').count(); q.onsuccess = () => { r.result.close(); resolve(q.result > 0); }; }; }))).toBe(true);
  // Общий черновик живёт на хабе: очистка устройства убирает только локальную копию.
  await expect.poll(() => api.draft(ids.pinned).text, { timeout: 10_000 }).toBe('Черновик второго окна');
  await other.evaluate(() => { (window as unknown as { __before?: boolean }).__before = true; });
  await page.locator('main').getByRole('button', { name: 'Очистить данные на устройстве', exact: true }).click();
  await expect(page.getByText('Данные на устройстве удалены', { exact: true })).toBeVisible();
  // Пока окно перезагружается, evaluate теряет контекст: такой опрос считаем ещё не завершённым.
  await expect.poll(() => other.evaluate(() => (window as unknown as { __before?: boolean }).__before === true).catch(() => true), { timeout: 10_000 }).toBe(false);
  await expect(other.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  expect(await other.evaluate(() => Object.keys(localStorage).filter(k => k.startsWith('canterlot.draft-tail.')))).toEqual([]);
  await expect(input).toHaveValue('Черновик второго окна');
  await other.close();
});

test('clearing_local_data_does_not_claim_the_session_expired', async ({ page }) => {
  await bootSecondary(page, '/settings');
  await page.locator('main').getByRole('button', { name: 'Очистить данные на устройстве', exact: true }).click();
  await expect(page.getByText('Данные на устройстве удалены', { exact: true })).toBeVisible();
  await page.waitForTimeout(1500);
  await expect(page.getByRole('dialog', { name: 'Сессия истекла' })).toBeHidden();
});
