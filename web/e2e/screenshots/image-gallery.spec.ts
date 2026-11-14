import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, test } from '../ui/fixtures';
import { FOUR, openViewer, seedFour, showChrome, thumb } from '../ui/image-gallery-fixture';
import { settled, viewer } from '../ui/image-viewer-fixture';

// Снимки общего просмотрщика с счётчиком сессии и кнопкой «Показать в чате»:
// PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/image-gallery.spec.ts --project=phone --project=desktop
const OUT = process.env.GALLERY_SHOTS_DIR ?? '/tmp/gallery-shots';
const PHONE = { width: 390, height: 844 };

test.describe('галерея сессии', () => {
  test.beforeAll(() => mkdirSync(OUT, { recursive: true }));

  for (const theme of ['light', 'dark'] as const) {
    test(`phone_${theme}_counter_and_reveal`, async ({ page }, info) => {
      test.skip(info.project.name !== 'phone', 'снимок телефона');
      test.setTimeout(120_000);
      await page.setViewportSize(PHONE);
      await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
      await seedFour(page);
      const dialog = await openViewer(page, thumb(page, FOUR.second));
      await expect(dialog.locator('.counter')).toHaveText('2 из 5');
      await expect(dialog.getByRole('button', { name: 'Показать в чате' })).toBeVisible();
      await showChrome(page);
      await settled(page);
      await page.screenshot({ path: resolve(OUT, `phone-390-${theme}.png`) });
    });

    test(`desktop_${theme}_counter_and_reveal`, async ({ page }, info) => {
      test.skip(info.project.name !== 'desktop', 'снимок рабочего стола');
      test.setTimeout(120_000);
      await page.setViewportSize({ width: 1440, height: 900 });
      await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
      await seedFour(page);
      const dialog = await openViewer(page, thumb(page, FOUR.second));
      await expect(dialog.locator('.counter')).toHaveText('2 из 5');
      await expect(dialog.getByRole('button', { name: 'Показать в чате' })).toBeVisible();
      await showChrome(page);
      await settled(page);
      await page.screenshot({ path: resolve(OUT, `desktop-1440-${theme}.png`) });
      await expect(viewer(page)).toBeVisible();
    });
  }
});
