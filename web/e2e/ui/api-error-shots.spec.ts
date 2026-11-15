import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { seedApiErrors } from '../../testing/fixtures/api-errors';
import { expect, test } from './fixtures';

// Снимки для ревью дизайна: только по запросу, в обычный прогон не входят.
const DIR = process.env.API_ERROR_SHOTS_DIR;

const SIZES = [
  { name: 'phone', width: 390, height: 844, phone: true },
  { name: 'desktop', width: 1440, height: 900, phone: false }
];

for (const size of SIZES) {
  for (const theme of ['light', 'dark'] as const) {
    test(`api_error_shot ${size.name} ${theme}`, async ({ page }) => {
      test.skip(!DIR, 'снимки по запросу');
      test.skip((test.info().project.name === 'phone') !== size.phone, 'размер своего проекта');
      await page.addInitScript((value) => {
        localStorage.setItem('canterlot.theme', value);
      }, theme);
      await page.setViewportSize({ width: size.width, height: size.height });
      const hub = new FakeHub();
      const api = new FakeHubApi(hub);
      const id = seedConversation(api, { title: 'Ошибки провайдера' });
      seedApiErrors(api, id);
      api.bindConversation(id);
      await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
      await page.goto(`/c/${id}`);
      await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

      mkdirSync(DIR!, { recursive: true });
      const base = join(DIR!, `${size.name}-${theme}`);
      const verify = page
        .locator('article.system[data-event-kind="api_error"]')
        .filter({ hasText: 'Claude: аккаунт требует проверки' });
      const overload = page
        .locator('article.system[data-event-kind="api_error"]')
        .filter({ hasText: 'Claude перегружен' });

      await verify.scrollIntoViewIfNeeded();
      await page.waitForTimeout(300);
      await verify.screenshot({ path: `${base}-403-folded.png` });
      await verify.getByRole('button', { name: 'Подробнее' }).click();
      await page.waitForTimeout(300);
      await verify.screenshot({ path: `${base}-403-expanded.png` });
      await overload.scrollIntoViewIfNeeded();
      await page.waitForTimeout(300);
      await overload.screenshot({ path: `${base}-529.png` });
      // Лента целиком: видно место пузырей и карточку с обычным уведомлением.
      await page.screenshot({ path: `${base}-feed.png`, fullPage: false });
    });
  }
}
