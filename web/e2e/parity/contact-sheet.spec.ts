import { test, expect } from '@playwright/test';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { buildContactSheet, CONTACT_DIR, MOCKUP_NAMES } from '../../scripts/contact-sheet.mjs';
import { bootScreen, SCREENS } from './states';

test('contact_sheet_covers_all_eleven_mockup_states', async ({ browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'phone', 'Снимки идут в одном движке: Chromium');
  test.setTimeout(180_000);
  const mockups = SCREENS.filter((screen) => screen.mockup);
  expect(mockups.map((screen) => screen.name)).toEqual(MOCKUP_NAMES);

  for (const screen of mockups) {
    const phone = screen.viewport.width < 1000;
    const context = await browser.newContext({ viewport: screen.viewport, deviceScaleFactor: phone ? 2 : 1, isMobile: phone, hasTouch: phone });
    const page = await context.newPage();
    await bootScreen(page, screen);
    await page.waitForTimeout(400);
    await page.screenshot({ path: join(CONTACT_DIR, `${screen.name}.png`) });
    await context.close();
  }

  const rows = buildContactSheet();
  expect(rows).toHaveLength(11);
  expect(existsSync(join(CONTACT_DIR, 'index.html'))).toBe(true);
  for (const row of rows) expect(row.ours, `снимок ${row.name}`).toBe(true);
});
