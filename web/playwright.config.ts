import { defineConfig, devices } from '@playwright/test';
const origin = `http://127.0.0.1:${Number(process.env.PORT ?? 4173)}`;
export default defineConfig({
  testDir: './e2e', fullyParallel: false, workers: process.argv.some(a => a.includes('e2e/stack')) ? 1 : 2,
  use: { baseURL: origin, trace: 'retain-on-failure' },
  webServer: { command: 'node e2e/edge/serve.mjs', url: origin, reuseExistingServer: false },
  projects: [
    { name: 'phone', use: { ...devices['Pixel 7'], browserName: 'chromium' } },
    { name: 'desktop', workers: 1, use: { browserName: 'firefox', viewport: { width: 1440, height: 900 } } }
  ]
});
