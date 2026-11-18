import { bootSecondary } from './secondary-fixture';
import { emitUpdate, FEED, makeUpdate, shot } from './updates-fixture';
import { expect, test } from './fixtures';

test('updates_banner_shows_newest_lines_closes_on_this_device_and_returns_on_a_newer_event', async ({ page, isMobile }, info) => {
  const { hub, api } = await bootSecondary(page, '/sessions', (api) => { api.updates = [...FEED]; });
  const banner = page.locator('[data-updates-banner]');

  await expect(banner).toBeVisible();
  await expect(banner.locator('[data-update-line]')).toHaveCount(3);
  await expect(banner.locator('[data-update-more]')).toHaveText('ещё 2');
  await expect(banner).toHaveAttribute('data-tone', 'warning');
  await expect(banner).toContainText('Доступна Codex 0.170 (установлена 0.169)');
  await expect(banner.getByRole('button', { name: 'Открыть ленту' })).toBeVisible();
  if (isMobile) await page.screenshot({ path: shot('banner-phone', info.project.name) });

  await banner.getByRole('button', { name: 'Закрыть' }).click();
  await expect(banner).toHaveCount(0);
  await page.reload();
  await expect(page.locator('[data-updates-banner]')).toHaveCount(0);

  emitUpdate(hub, api, makeUpdate(99, 'cli_updated', 0, { from: '0.169', to: '0.170' }));
  await expect(page.locator('[data-updates-banner]')).toBeVisible();
  await expect(page.locator('[data-updates-banner]')).toContainText('обновился сам');
});

test('updates_banner_arrives_live_without_polling_and_opens_the_feed', async ({ page }) => {
  const { hub, api } = await bootSecondary(page, '/sessions', (api) => {
    api.updates = [makeUpdate(1, 'models_added', 10, { models: ['model-1'] })];
  });
  const banner = page.locator('[data-updates-banner]');
  await expect(banner).toBeVisible();
  await banner.getByRole('button', { name: 'Закрыть' }).click();
  await expect(banner).toHaveCount(0);

  const callsBefore = api.updatesCalls.length;
  emitUpdate(hub, api, makeUpdate(2, 'below_minimum', 0, {
    installed_version: '2.2.4', min_version: '2.2.6'
  }, { harness: 'opencode' }));
  await expect(banner).toBeVisible();
  await expect(banner).toContainText('Ниже поддерживаемой версии: OpenCode 2.2.4, нужна 2.2.6');
  await expect(banner).toHaveAttribute('data-tone', 'warning');
  // Живое событие не вызвало лишний запрос ленты.
  expect(api.updatesCalls.length).toBe(callsBefore);

  await banner.getByRole('button', { name: 'Открыть ленту' }).click();
  await expect(page).toHaveURL(/\/settings\?c=updates$/);
  await expect(page.locator('[data-update-feed]')).toBeVisible();
});
