import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedWaitingSessions } from '../../testing/fixtures/waiting-sessions';
import { test } from './fixtures';

// Снимки для ревью дизайна: только по запросу, в обычный прогон не входят.
const DIR = process.env.WAITING_SHOTS_DIR;

async function boot(page: Page, width: number, height: number, theme: string) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedWaitingSessions(api);
  await page.addInitScript((value) => { localStorage.setItem('canterlot.theme', value); }, theme);
  await page.setViewportSize({ width, height });
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');
  return ids;
}

const SIZES = [
  { name: 'phone-360', width: 360, height: 740 },
  { name: 'phone-390', width: 390, height: 844 },
  { name: 'desktop', width: 1440, height: 900 }
];

for (const size of SIZES) {
  const phone = size.name.startsWith('phone');
  for (const theme of ['light', 'dark']) {
    test(`waiting_shot ${size.name} ${theme}`, async ({ page }) => {
      test.skip(!DIR, 'снимки по запросу');
      test.skip((test.info().project.name === 'phone') !== phone, 'размер своего проекта');
      const ids = await boot(page, size.width, size.height, theme);
      await page.goto('/sessions');
      await page.locator('[data-list-ready]').waitFor({ timeout: 15_000 });
      await page.getByRole('button', { name: 'Ждут вас · 2' }).waitFor();
      await page.waitForTimeout(800);
      mkdirSync(DIR!, { recursive: true });
      await page.screenshot({ path: join(DIR!, `list-${size.name}-${theme}.png`) });

      // Шапка чата со счётом чужих ожиданий: на телефоне есть стрелка назад.
      if (!phone) return;
      await page.goto(`/c/${ids.ids.current}`);
      await page.locator('[data-ready]').waitFor({ timeout: 15_000 });
      await page.getByRole('link', { name: 'Назад, ждут ответа: 2' }).waitFor();
      await page.waitForTimeout(800);
      await page.screenshot({ path: join(DIR!, `chat-${size.name}-${theme}.png`) });
    });
  }
}
