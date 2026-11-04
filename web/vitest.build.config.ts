import { defineConfig } from 'vitest/config';
import unitConfig from './vitest.config.ts';

// Проверки собранных артефактов: нужна готовая production-сборка в build/.
// check-web.sh запускает этот прогон после сборок, отдельно от основного vitest.
export default defineConfig({
  ...unitConfig,
  test: {
    ...unitConfig.test,
    include: ['tests/build/**/*.test.ts']
  }
});
