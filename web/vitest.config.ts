import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { workspaceVersion } from './scripts/workspace-version.mjs';

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(workspaceVersion())
  },
  plugins: [svelte({ configFile: false })],
  server: {
    fs: { allow: ['..'] }
  },
  resolve: {
    conditions: ['browser'],
    alias: {
      $lib: new URL('./src/lib', import.meta.url).pathname,
      '#lib': new URL('./src/lib', import.meta.url).pathname
    }
  },
  test: {
    include: ['tests/unit/**/*.test.ts'],
    environment: 'jsdom',
    maxWorkers: '75%',
    testTimeout: 20000
  }
});
