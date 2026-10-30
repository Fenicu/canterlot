import { test, expect } from './fixtures';
import { bootSecondary, postShare } from './secondary-fixture';

test.beforeEach(({ browserName }, info) => { test.skip(info.title.includes('@chromium') && browserName !== 'chromium', 'Проверка Chromium'); });

test('share_target_post_reaches_the_session_picker_with_the_files @chromium', async ({ page }) => {
  await bootSecondary(page, '/sessions');
  const url = await postShare(page); await page.goto(url);
  await expect(page.getByRole('heading', { name: 'Отправить в canterlot' })).toBeVisible();
  await expect(page.locator('main').getByText('notes.txt')).toBeVisible();
  await page.getByRole('searchbox', { name: 'Выберите сессию' }).fill('Потока');
  await expect(page.locator('main').getByRole('button', { name: /Восстановление потока/ })).toBeVisible();
  await expect(page.locator('main').getByRole('button', { name: /Очередь сообщений/ })).toHaveCount(0);
  await expect(page.locator('main').getByRole('link', { name: 'Новая сессия' })).toBeVisible();
});

test('session_picker_attaches_files_to_the_chosen_conversation_and_starts_uploads @chromium', async ({ page }) => {
  const { ids } = await bootSecondary(page, `/sessions`);
  await page.goto(`/c/${ids.pinned}`);
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).fill('Черновик');
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).blur();
  const url = await postShare(page); await page.goto(url);
  const upload = page.waitForRequest(r => r.method() === 'POST' && r.url().endsWith('/attachments'));
  await page.locator('main').getByRole('button', { name: /Восстановление потока/ }).click();
  expect((await upload).postDataJSON().conversation_id).toBe(ids.pinned);
  await expect(page).toHaveURL(new RegExp(`/c/${ids.pinned}$`));
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Черновик\nОписание\nСверь с кодом\nhttps://example.org/');
  await expect(page.getByRole('progressbar', { name: 'notes.txt' })).toHaveAttribute('aria-valuenow', '7');
  await page.goto(url);
  await expect(page.getByText('Переданные данные не найдены или срок хранения истёк')).toBeVisible();
});

test('share_without_an_active_service_worker_shows_the_explanation', async ({ page }) => {
  await bootSecondary(page, '/share?error=sw_inactive');
  await expect(page.getByText('Приложение ещё не готово принять файл: откройте canterlot и поделитесь ещё раз')).toBeVisible();
});

test('share_picker_reuses_composer_validation_and_keeps_the_text @chromium', async ({ page }) => {
  const { ids } = await bootSecondary(page, '/sessions');
  await page.evaluate(async () => { await navigator.serviceWorker.ready; });
  await page.waitForFunction(() => !!navigator.serviceWorker.controller);
  const url = await page.evaluate(async () => {
    const form = new FormData(); form.set('text', 'Текст с пустым файлом');
    form.append('files', new File([], 'empty.txt'));
    return (await fetch('/_share', { method: 'POST', body: form })).url;
  });
  await page.goto(url);
  await page.locator('main').getByRole('button', { name: /Восстановление потока/ }).click();
  await expect(page).toHaveURL(new RegExp(`/c/${ids.pinned}$`));
  await expect(page.getByText('Файл пустой', { exact: true })).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Текст с пустым файлом');
});

test('share_picker_can_create_a_session_without_sending_the_draft @chromium', async ({ page }) => {
  await bootSecondary(page, '/sessions');
  const url = await postShare(page); await page.goto(url);
  await page.locator('main').getByRole('link', { name: 'Новая сессия' }).click();
  await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await page.getByRole('textbox', { name: 'Рабочая папка' }).fill('/srv');
  await page.getByRole('radio', { name: 'Claude Code' }).check();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('opus');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  let sent = 0;
  page.on('request', request => { if (request.method() === 'POST' && request.url().endsWith('/inputs')) sent++; });
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Описание\nСверь с кодом\nhttps://example.org/');
  await expect(page.getByRole('progressbar', { name: 'notes.txt' })).toHaveAttribute('aria-valuenow', '7');
  expect(sent).toBe(0);
});
