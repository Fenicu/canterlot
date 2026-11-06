import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { test } from './fixtures';
import { bootRealisticLimits, SHOT_SIZES } from './limits-seed';

// Снимки для ревью дизайна: только по запросу, в обычный прогон не входят.
const DIR = process.env.LIMITS_SHOTS_DIR;
const STAGE = process.env.LIMITS_SHOTS_STAGE ?? 'after';

for (const size of SHOT_SIZES) {
  const phone = size.name.startsWith('phone');
  for (const theme of phone ? ['dark'] : ['light', 'dark']) {
    for (const history of ['full', 'empty'] as const) {
      test(`limits_shot ${size.name} ${theme} ${history}`, async ({ page }) => {
        test.skip(!DIR, 'снимки по запросу');
        test.skip((test.info().project.name === 'phone') !== phone, 'размер своего проекта');
        await page.addInitScript((value) => { localStorage.setItem('canterlot.theme', value); }, theme);
        await page.setViewportSize({ width: size.width, height: size.height });
        await bootRealisticLimits(page, { history });
        await page.waitForTimeout(800);
        mkdirSync(DIR!, { recursive: true });
        const base = join(DIR!, `${STAGE}-${size.name}-${theme}${history === 'empty' ? '-empty' : ''}`);
        await page.screenshot({ path: `${base}.png` });
        // Весь экран целиком: тело экрана прокручивается само, поэтому окно растягивается до высоты содержимого.
        const height = await page.evaluate(() => {
          const body = document.querySelector('main .secondary-body') as HTMLElement | null;
          return body ? body.scrollHeight - body.clientHeight + window.innerHeight : window.innerHeight;
        });
        if (height > size.height) {
          await page.setViewportSize({ width: size.width, height });
          await page.waitForTimeout(300);
          await page.screenshot({ path: `${base}-full.png` });
        }
      });
    }
  }
}
