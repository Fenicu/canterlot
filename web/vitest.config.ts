import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    conditions: ['browser'],
    alias: {
      $lib: new URL('./src/lib', import.meta.url).pathname
    }
  },
  test: {
    include: ['tests/unit/**/*.test.ts'],
    environment: 'jsdom',
    maxWorkers: 2,
    testTimeout: 20000
  }
});
