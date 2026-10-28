import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { removeKitAnnouncer } from './scripts/kit-announcer.mjs';
import { directives } from './csp.mjs';
export default {
  preprocess: [{ markup: removeKitAnnouncer }, vitePreprocess()],
  kit: { adapter: adapter({ fallback: '200.html', strict: false }), csp: { mode: 'hash', directives } }
};
