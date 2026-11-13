import { defineConfig, devices } from '@playwright/test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fakeAudioFile } from './testing/fake-audio.mjs';

const STACK_INFO_FILE = join(tmpdir(), 'canterlot-stack-info.json');
// Cookie владельца, которую global-setup стенда получает по ссылке восстановления.
const STACK_STATE_FILE = join(tmpdir(), 'canterlot-stack-state.json');
// Процессы-исполнители перечитывают конфиг без аргументов командной строки: признак стенда идёт им через окружение.
const isStackOrPerf = process.env.CANTERLOT_STACK_RUN === '1' || process.argv.some(a => a.includes('e2e/stack') || a.includes('e2e/perf'));
if (isStackOrPerf) process.env.CANTERLOT_STACK_RUN = '1';
// Скриншоты тем и README снимаются отдельно: в обычный прогон они не входят.
const screenshots = process.env.PW_SCREENSHOTS === '1';

let origin = `http://127.0.0.1:${Number(process.env.PORT ?? 4173)}`;
if (isStackOrPerf && existsSync(STACK_INFO_FILE)) {
  try {
    origin = JSON.parse(readFileSync(STACK_INFO_FILE, 'utf8')).baseUrl;
  } catch {}
}

const requestedWorkers = process.env.PW_WORKERS;
const workers = requestedWorkers ? (requestedWorkers.endsWith('%') ? requestedWorkers : Number(requestedWorkers)) : '50%';

const phone = { ...devices['Pixel 7'], browserName: 'chromium' as const };
// Настоящий worklet проверяется отдельным проектом: ему нужны поддельный микрофон и тон из файла.
const DICTATION_MEDIA_SPEC = /dictation-worklet\.spec\.ts$/;
const dictationAudio = fakeAudioFile();
const LAYOUT_SPEC = /desktop-layout\.spec\.ts$/;
const firefoxLayout = (name: string, width: number, height: number) => ({
  name,
  workers: 1,
  testMatch: LAYOUT_SPEC,
  testIgnore: [],
  use: { browserName: 'firefox' as const, viewport: { width, height } }
});

export default defineConfig({
  testDir: './e2e',
  // Проект budget зависит от phone и desktop, поэтому прогон `e2e/ui` тянул бы за собой и сквозные наборы, которым нужен стенд.
  testIgnore: isStackOrPerf
    ? LAYOUT_SPEC
    : ['**/e2e/stack/**', '**/e2e/perf/**', ...(screenshots ? [] : ['**/e2e/screenshots/**']), LAYOUT_SPEC],
  // Тесты независимы: пара «проект, тест» занимает воркер сама. Стенд и перф делят одно состояние и идут по одному.
  fullyParallel: !isStackOrPerf,
  workers: isStackOrPerf ? 1 : workers,
  globalSetup: isStackOrPerf ? './e2e/stack/global-setup.ts' : undefined,
  globalTeardown: isStackOrPerf ? './e2e/stack/global-teardown.ts' : undefined,
  use: { baseURL: origin, trace: 'retain-on-failure', storageState: isStackOrPerf ? STACK_STATE_FILE : undefined },
  webServer: isStackOrPerf
    ? undefined
    : { command: 'node e2e/edge/serve.mjs', url: origin, reuseExistingServer: false },
  projects: [
    { name: 'phone', grepInvert: /render_budget_during_long_stream|owner_messages_jump_long_task/, testIgnore: DICTATION_MEDIA_SPEC, use: phone },
    { name: 'desktop', workers: isStackOrPerf ? 1 : undefined, testIgnore: DICTATION_MEDIA_SPEC, use: { browserName: 'firefox', viewport: { width: 1440, height: 900 } } },
    {
      name: 'dictation-media',
      testMatch: DICTATION_MEDIA_SPEC,
      use: {
        ...phone,
        launchOptions: {
          args: [
            '--use-fake-device-for-media-stream',
            '--use-fake-ui-for-media-stream',
            `--use-file-for-fake-audio-capture=${dictationAudio}`
          ]
        }
      }
    },
    firefoxLayout('layout-1024', 1024, 768),
    firefoxLayout('layout-1440', 1440, 900),
    firefoxLayout('layout-2560', 2560, 1440),
    firefoxLayout('layout-3840', 3840, 2160),
    // Дроссель 4× считает время кадра по настенным часам: соседний браузер на этой машине раздувает задачу за 200 мс.
    { name: 'budget', workers: 1, dependencies: ['phone', 'desktop'], grep: /render_budget_during_long_stream|owner_messages_jump_long_task/, use: phone }
  ]
});
