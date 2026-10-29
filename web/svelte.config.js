import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { removeKitAnnouncer } from './scripts/kit-announcer.mjs';
import { directives } from './csp.mjs';
const version = process.env.PWA_VERSION ??= String(Date.now());
export default {
  preprocess: [{ markup: removeKitAnnouncer }, vitePreprocess()],
  kit: {
    adapter: adapter({ pages: process.env.PWA_BUILD_DIR ?? 'build', assets: process.env.PWA_BUILD_DIR ?? 'build', fallback: '200.html', strict: false }),
    serviceWorker: { register: false },
    version: { name: version },
    csp: { mode: 'hash', directives }
  }
};
