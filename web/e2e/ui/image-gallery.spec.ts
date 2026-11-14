import { expect, test } from './fixtures';
import { FOUR, openViewer, phone, seedFour, seedMany, showChrome, swipe, thumb } from './image-gallery-fixture';
import { settled, viewer } from './image-viewer-fixture';

// Хаб поддельный, но машину делят соседние прогоны: загрузка модулей и картинок бывает медленной.
test.describe.configure({ timeout: 120_000 });

test('an_image_in_the_middle_of_the_session_swipes_across_a_message_boundary', async ({ page }, info) => {
  await seedFour(page);
  const dialog = await openViewer(page, thumb(page, FOUR.second));
  // Место во всей сессии, а не в своём сообщении: до второго сообщения одна картинка.
  await expect(dialog.locator('.counter')).toHaveText('2 из 5');
  await swipe(page, phone(info.project.name), 'newer');
  await expect(dialog.locator('.counter')).toHaveText('3 из 5');
  await expect(dialog.locator('.slide[data-current] img.full')).toHaveAttribute('src', '/api/v1/media/g-3');
  await swipe(page, phone(info.project.name), 'newer');
  await expect(dialog.locator('.counter')).toHaveText('4 из 5');
  await expect(dialog.locator('.slide[data-current] img.full')).toHaveAttribute('src', '/api/v1/attachments/g-4/content');
  await swipe(page, phone(info.project.name), 'newer');
  await expect(dialog.locator('.counter')).toHaveText('5 из 5');
  // За последней картинкой сессии лента только пружинит.
  await swipe(page, phone(info.project.name), 'newer');
  await expect(dialog.locator('.counter')).toHaveText('5 из 5');
});

test('the_counter_starts_at_the_tapped_picture_and_shows_the_session_total', async ({ page }) => {
  await seedFour(page);
  const dialog = await openViewer(page, thumb(page, FOUR.first));
  await expect(dialog.locator('.counter')).toHaveText('1 из 5');
  await page.keyboard.press('Escape');
  await expect(viewer(page)).toHaveCount(0);
  const file = await openViewer(page, thumb(page, FOUR.file));
  await expect(file.locator('.counter')).toHaveText('4 из 5');
  await expect(file.locator('.caption')).toContainText('Снимок 4');
});

test('desktop_arrow_keys_page_across_messages', async ({ page }, info) => {
  test.skip(phone(info.project.name), 'стрелки проверяются на рабочем столе');
  await seedFour(page);
  const dialog = await openViewer(page, thumb(page, FOUR.second));
  await page.keyboard.press('ArrowRight');
  await expect(dialog.locator('.counter')).toHaveText('3 из 5');
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(dialog.locator('.counter')).toHaveText('1 из 5');
  await expect(dialog.getByRole('button', { name: 'Предыдущая картинка' })).toHaveAttribute('aria-disabled', 'true');
});

test('closing_returns_to_the_same_place_in_the_chat', async ({ page }) => {
  const seed = await seedMany(page);
  // Восьмидесятое сообщение: над ним в окне ленты есть строки, прокрутка не нулевая.
  await page.goto(`/c/${seed.id}?input=${seed.inputs[79]}&seq=160`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 60_000 });
  const row = page.locator('[data-key="input:input-80"]');
  await expect(row).toBeVisible();
  await settled(page);
  const feed = page.locator('.feed');
  const before = await feed.evaluate((node) => node.scrollTop);
  expect(before).toBeGreaterThan(0);
  const dialog = await openViewer(page, row.locator('a.thumb'));
  await expect(dialog.locator('.counter')).toHaveText('80 из 130');
  await page.keyboard.press('Escape');
  await expect(viewer(page)).toHaveCount(0);
  await settled(page);
  const after = await feed.evaluate((node) => node.scrollTop);
  expect(Math.abs(after - before)).toBeLessThanOrEqual(1);
});

test('the_viewer_pages_into_history_the_timeline_has_not_loaded_and_reveals_it', async ({ page }, info) => {
  const seed = await seedMany(page);
  // Окно ленты встаёт вокруг 40-го сообщения: 20-го в нём нет.
  await page.goto(`/c/${seed.id}?input=${seed.inputs[39]}&seq=80`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 60_000 });
  const target = page.locator('[data-key="input:input-20"]');
  await expect(target).toHaveCount(0);
  const dialog = await openViewer(page, page.locator('[data-key="input:input-40"] a.thumb'));
  await expect(dialog.locator('.counter')).toHaveText('40 из 130');
  // Старше 40-го просмотрщик догружает страницу, которой в ленте ещё нет.
  for (let step = 0; step < 40; step++) {
    if ((await dialog.locator('.counter').textContent())?.trim() === '20 из 130') break;
    await swipe(page, phone(info.project.name), 'older');
  }
  await expect(dialog.locator('.counter')).toHaveText('20 из 130');
  await expect(target).toHaveCount(0);
  // Назад к младшим кадрам — по одному, а не на последний кадр догруженной страницы.
  await swipe(page, phone(info.project.name), 'newer');
  await expect(dialog.locator('.counter')).toHaveText('21 из 130');
  await swipe(page, phone(info.project.name), 'older');
  await expect(dialog.locator('.counter')).toHaveText('20 из 130');
  await showChrome(page);
  await dialog.getByRole('button', { name: 'Показать в чате' }).click();
  await expect(viewer(page)).toHaveCount(0);
  await expect(target).toBeVisible();
  await expect(target.locator('.bubble[data-jump]')).toBeVisible();
  await expect(target.locator('a.thumb')).toBeVisible();
});
