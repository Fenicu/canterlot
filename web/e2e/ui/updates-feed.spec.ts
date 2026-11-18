import { bootSecondary } from './secondary-fixture';
import { FEED, makeUpdate, shot, UPDATE_COMMAND } from './updates-fixture';
import { expect, test } from './fixtures';

test('updates_feed_shows_all_kinds_command_copy_filters_and_second_page', async ({ page }) => {
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']).catch(() => {});
  const older = Array.from({ length: 18 }, (_, index) =>
    makeUpdate(10 + index, 'models_added', 10 + index, { models: [`model-${index}`] })
  );
  const seeded = [...FEED, ...older];
  const { api } = await bootSecondary(page, '/settings?c=updates', (api) => { api.updates = seeded; });
  const feed = page.locator('[data-update-feed]');

  await expect(feed.locator('[data-update-item]')).toHaveCount(20);
  for (const text of [
    'Доступна Codex 0.170 (установлена 0.169)',
    'Ниже поддерживаемой версии: OpenCode 2.2.4, нужна 2.2.6',
    'Claude Code обновился сам: 2.1.311 → 2.1.330',
    'Добавлены модели: claude-fable-5-5, gpt-6.1-sol',
    'Убраны модели: gpt-6.1-sol'
  ]) await expect(feed).toContainText(text);
  await expect(feed.locator('[data-update-item]').first()).toContainText('workstation');
  await expect(feed.locator('[data-update-kind="version_available"]')).toContainText('Codex');
  // Дата и время в локальной русской подписи, а не сырой RFC 3339.
  await expect(feed.locator('[data-update-item]').first()).toContainText(/ноября.*\d{2}:\d{2}/);

  const command = feed.locator('[data-update-command]').first();
  await expect(command).toHaveText(UPDATE_COMMAND);
  await expect(command).toHaveCSS('font-family', /mono|Consolas|Liberation/i);
  const first = feed.locator('[data-update-kind="version_available"]');
  await first.getByRole('button', { name: 'Копировать' }).click();
  await expect(first.getByRole('status')).toHaveText('Скопировано');
  const clipboard = await page.evaluate(() => navigator.clipboard.readText()).catch(() => '');
  if (clipboard) expect(clipboard).toBe(UPDATE_COMMAND);

  // Второй лист приходит по курсору: прокрутка до метки продолжения больше не требует кнопки.
  await feed.locator('.more').scrollIntoViewIfNeeded();
  await expect(feed.locator('[data-update-item]')).toHaveCount(seeded.length);
  await expect.poll(() => api.updatesCalls.some((call) => Boolean(call.cursor))).toBe(true);

  await feed.getByRole('combobox', { name: 'Событие' }).selectOption('models_added');
  await expect.poll(() => api.updatesCalls.at(-1)?.kind).toBe('models_added');
  await expect(feed.locator('[data-update-item]:not([data-update-kind="models_added"])')).toHaveCount(0);
  await expect(feed.locator('[data-update-item]')).toHaveCount(19);
});

test('updates_feed_empty_state_is_spelled_out', async ({ page }) => {
  await bootSecondary(page, '/settings?c=updates');
  await expect(page.locator('[data-update-empty]')).toHaveText('Пока нет обновлений и изменений моделей');
});

test('updates_feed_phone_shot', async ({ page, isMobile }, info) => {
  test.skip(!isMobile, 'снимок ленты на телефоне');
  await bootSecondary(page, '/settings?c=updates', (api) => { api.updates = FEED; });
  await expect(page.locator('[data-update-feed] [data-update-item]')).toHaveCount(FEED.length);
  await page.screenshot({ path: shot('feed-phone', info.project.name), fullPage: true });
});
