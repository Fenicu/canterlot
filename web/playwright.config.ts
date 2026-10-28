import { defineConfig, devices } from '@playwright/test';
export default defineConfig({
  testDir: './e2e', fullyParallel: true, workers: process.argv.some(a => a.includes('e2e/stack')) ? 1 : 2,
  use: { baseURL: 'http://127.0.0.1:4173', trace: 'retain-on-failure' },
  webServer: { command: 'node e2e/edge/serve.mjs', url: 'http://127.0.0.1:4173', reuseExistingServer: false },
  projects: [
    { name: 'phone', use: { ...devices['Pixel 7'], browserName: 'chromium' } },
    { name: 'desktop', use: { browserName: 'firefox', viewport: { width: 1440, height: 900 } } }
  ]
});
