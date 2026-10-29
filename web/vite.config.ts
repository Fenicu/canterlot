import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';
import { pwaManifest } from './scripts/gen-manifest.mjs';
export default defineConfig({
  plugins: [pwaManifest(), sveltekit()],
  server: { proxy: { '/api': {
    target: process.env.HUB_URL ?? 'http://127.0.0.1:8080', ws: true,
    headers: { 'Remote-User': process.env.HUB_OWNER ?? 'owner', Origin: process.env.HUB_ALLOWED_ORIGIN ?? 'http://localhost:5173' }
  } } }
});
