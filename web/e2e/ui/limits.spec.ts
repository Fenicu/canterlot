import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

test('limits_history_follows_mockup_10_with_an_accessible_data_table', async ({ page }) => {
  const { historyRequests } = await bootSecondary(page, '/limits');
  const main = page.locator('main');
  await expect(main.getByRole('img', { name: /Дневные снимки лимитов/ })).toBeVisible();
  await expect(main.locator('[data-series="five"]').first()).toHaveCSS('height', /.+/);
  await expect(main.locator('[data-series="weekly"]')).toHaveCount(14);
  const table = main.getByRole('table', { name: 'Дневные снимки лимитов' });
  await expect(table).toHaveCount(1);
  await expect(table.getByRole('row')).toHaveCount(15);
  await expect(main.getByText('Claude сейчас')).toBeVisible();
  await expect(main.getByText('Codex сейчас')).toBeVisible();
  await expect(main.getByText('Субагент · haiku').first()).toBeVisible();
  await expect(main.getByText('приблизительно', { exact: true }).first()).toBeVisible();
  await expect(main.getByText(/Кэш включён во входные токены/)).toBeVisible();
  expect(historyRequests).toHaveLength(2);
  for (const query of historyRequests) { expect(query.get('tz')).toBeTruthy(); expect(query.get('from')).toMatch(/^\d{4}-\d{2}-\d{2}T/); expect(query.get('to')).toMatch(/^\d{4}-\d{2}-\d{2}T/); }
});
