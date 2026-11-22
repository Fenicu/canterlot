import { defineConfig, devices } from '@playwright/test';

// Браузерные тесты удалены 22.11.2026 по решению владельца. Обвязка осталась
// для разовых проверок экрана и снимков: см. «Разовая проверка экрана в браузере»
// в docs/development.md. Скриншоты README снимаются отдельным режимом.
const screenshots = process.env.PW_SCREENSHOTS === '1';
const origin = `http://127.0.0.1:${Number(process.env.PORT ?? 4173)}`;

export default defineConfig({
    testDir: './e2e',
    // Снимки README пишутся в docs/screenshots и в обычный прогон не входят.
    testIgnore: screenshots ? [] : ['**/e2e/screenshots/**'],
    use: { baseURL: origin, trace: 'retain-on-failure' },
    webServer: { command: 'node e2e/edge/serve.mjs', url: origin, reuseExistingServer: false },
    projects: [
        { name: 'phone', use: { ...devices['Pixel 7'], browserName: 'chromium' as const } },
        { name: 'desktop', use: { browserName: 'firefox' as const, viewport: { width: 1440, height: 900 } } }
    ]
});
