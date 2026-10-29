import { defineConfig, devices } from '@playwright/test';
const origin = `http://127.0.0.1:${Number(process.env.PORT ?? 4173)}`;
const phone = { ...devices['Pixel 7'], browserName: 'chromium' as const };
export default defineConfig({
  testDir: './e2e', fullyParallel: false, workers: process.argv.some(a => a.includes('e2e/stack')) ? 1 : 4,
  use: { baseURL: origin, trace: 'retain-on-failure' },
  webServer: { command: 'node e2e/edge/serve.mjs', url: origin, reuseExistingServer: false },
  projects: [
    { name: 'phone', grepInvert: /render_budget_during_long_stream/, use: phone },
    { name: 'desktop', workers: 2, use: { browserName: 'firefox', viewport: { width: 1440, height: 900 } } },
    // Дроссель 4× считает время кадра по настенным часам: соседний браузер на этой машине раздувает задачу за 200 мс.
    { name: 'budget', workers: 1, dependencies: ['phone', 'desktop'], grep: /render_budget_during_long_stream/, use: phone }
  ]
});
