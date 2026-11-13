// Снимки панели и настроек диктовки для ревью: `PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/dictation-shots.spec.ts --project=phone --project=desktop`.
// Файлы ложатся в DICTATION_SHOTS_OUT (по умолчанию /tmp/dictation-shots).
import { mkdirSync } from 'node:fs';
import { expect, test } from '../ui/fixtures';
import { bootSecondary } from '../ui/secondary-fixture';
import { bootDictation, panel, startDictation } from '../ui/dictation-fixture';

test.skip(process.env.PW_SCREENSHOTS !== '1', 'снимки для ревью снимаются отдельно');

const DIR = process.env.DICTATION_SHOTS_OUT ?? '/tmp/dictation-shots';
if (process.env.PW_SCREENSHOTS === '1') mkdirSync(DIR, { recursive: true });

const shot = (name: string, project: string) => `${DIR}/${name}-${project}.png`;

test('dictation_panel_listening_with_a_tentative_tail', async ({ page }, info) => {
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = {
        texts: [
          { afterMs: 20, committed: 'Открой ветку и проверь ', tentative: 'маршрутизацию' }
        ]
      };
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-tentative]')).toHaveText('маршрутизацию');
  await panel(page).screenshot({ path: shot('dictation-listening', info.project.name) });
});

test('dictation_panel_slow_mode', async ({ page }, info) => {
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'cpu', engine: 'warm' };
      dictation.mode = 'cpu';
      dictation.script = {
        texts: [{ afterMs: 20, committed: 'Проверка медленного режима', tentative: '' }]
      };
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-slow-mode]')).toBeVisible();
  await expect(page.locator('[data-dictation-committed]')).toHaveText('Проверка медленного режима');
  await panel(page).screenshot({ path: shot('dictation-slow-mode', info.project.name) });
});

test('dictation_panel_connection_lost_with_two_choices', async ({ page }, info) => {
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = {
        texts: [{ afterMs: 20, committed: 'Связь оборвалась на середине', tentative: ' фразы' }],
        closeAfterMs: 120
      };
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-state]')).toHaveText('Связь прервалась');
  await expect(page.getByRole('button', { name: 'Вставить что есть (неполно)' })).toBeVisible();
  await panel(page).screenshot({ path: shot('dictation-connection-lost', info.project.name) });
});

test('dictation_settings_block', async ({ page }, info) => {
  await bootSecondary(page, '/settings?c=input', (api) => {
    api.hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    api.hub.dictation.terms = [
      { pattern: 'мейн', replacement: 'main' },
      { pattern: 'апи', replacement: 'API' },
      { pattern: 'плейврайт', replacement: 'Playwright' }
    ];
  });
  const block = page.locator('[data-dictation-settings]');
  await expect(block.locator('[data-dictation-term]')).toHaveCount(3);
  await block.scrollIntoViewIfNeeded();
  await block.screenshot({ path: shot('dictation-settings', info.project.name) });
});
