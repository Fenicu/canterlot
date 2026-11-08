// Снимки отчёта сторожа на экране машин для ревью:
// `PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/machine-watch.spec.ts --project=phone`.
// Файлы ложатся в MACHINE_WATCH_OUT (по умолчанию /tmp/watchdog).
import { test, expect } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { seedMachineWatch } from '../../testing/fake-hub';
import { bootSecondary } from '../ui/secondary-fixture';

const OUT = process.env.MACHINE_WATCH_OUT ?? '/tmp/watchdog';

test.skip(process.env.PW_SCREENSHOTS !== '1', 'снимки для ревью снимаются отдельно');

for (const shot of [
  { file: 'machines-phone-dark', width: 390, height: 844, theme: 'dark' as const },
  { file: 'machines-desktop-light', width: 1280, height: 800, theme: 'light' as const }
]) {
  test(`machine_watch_screenshot: ${shot.file}`, async ({ browser }) => {
    mkdirSync(OUT, { recursive: true });
    const context = await browser.newContext({ viewport: { width: shot.width, height: shot.height }, deviceScaleFactor: 2, colorScheme: shot.theme, isMobile: shot.width < 600, hasTouch: shot.width < 600 });
    await context.addInitScript((value) => localStorage.setItem('canterlot.theme', value), shot.theme);
    const page = await context.newPage();
    const { api, ids } = await bootSecondary(page);
    seedMachineWatch(api, ids.machine);
    await page.reload();
    await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('html')).toHaveAttribute('data-theme', shot.theme);
    const caches = page.getByRole('region', { name: 'Крупные кэши сборки' });
    await expect(caches).toBeVisible();
    await page.screenshot({ path: `${OUT}/${shot.file}.png` });
    // Блок ниже первого экрана: прокручиваемая панель снимается частями до конца отчёта.
    const body = page.locator('.secondary-body');
    const sizes = await body.evaluate((node) => ({ scroll: node.scrollHeight, client: node.clientHeight }));
    let part = 2;
    for (let top = sizes.client * 0.85; top < sizes.scroll - sizes.client * 0.15; top += sizes.client * 0.85) {
      await body.evaluate((node, y) => { node.scrollTop = y; }, top);
      await page.waitForTimeout(150);
      await page.screenshot({ path: `${OUT}/${shot.file}-${part}.png` });
      part += 1;
    }
    await context.close();
  });
}
