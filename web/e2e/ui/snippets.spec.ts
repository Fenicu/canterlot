import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';

const TITLE = 'Проверка очереди';
const BODY = 'Посмотри, что сейчас в очереди задач';

for (const project of ['phone', 'desktop']) {
  test(`snippet_created_in_settings_inserted_from_composer_${project}`, async ({ page }, info) => {
    test.skip(info.project.name !== project, 'другой проект');
    const { ids } = await bootSecondary(page, '/settings?c=input');
    await expect(page.getByRole('heading', { name: 'Заготовки сообщений' })).toBeVisible();

    await page.getByRole('textbox', { name: 'Название заготовки' }).first().fill(TITLE);
    await page.getByRole('textbox', { name: 'Текст заготовки' }).fill(BODY);
    await page.getByRole('button', { name: 'Добавить заготовку' }).click();
    await expect(page.locator('.list .label').filter({ hasText: TITLE })).toBeVisible();

    await page.goto(`/c/${ids.pinned}`);
    const field = page.getByRole('textbox', { name: 'Сообщение' });
    await expect(field).toBeVisible();
    await page.getByRole('button', { name: 'Заготовки' }).click();
    const sheet = page.getByRole('dialog');
    await expect(sheet.getByRole('button', { name: TITLE })).toBeVisible();
    await sheet.getByRole('button', { name: TITLE }).click();
    // Вставка не отправляет сообщение: после неё текст остаётся в поле ввода.
    await expect(field).toHaveValue(BODY);
  });

  test(`snippet_live_update_from_second_page_${project}`, async ({ page, context }, info) => {
    test.skip(info.project.name !== project, 'другой проект');
    const { hub, api, ids } = await bootSecondary(page, '/settings');
    await page.goto(`/c/${ids.pinned}`);
    const other = await context.newPage();
    await installChatRoutes(other, hub, api, ids.pinned);
    await other.goto(`/c/${ids.pinned}`);
    await expect(other.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();

    await page.getByRole('button', { name: 'Заготовки' }).click();
    await expect(page.getByRole('dialog')).toBeVisible();
    api.createMessageSnippet({ scope: 'global', title: TITLE, body: BODY });
    await expect(page.getByRole('dialog').getByRole('button', { name: TITLE })).toBeVisible();
    await other.getByRole('button', { name: 'Заготовки' }).click();
    await expect(other.getByRole('dialog').getByRole('button', { name: TITLE })).toBeVisible();
    await other.close();
  });
}
