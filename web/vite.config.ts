import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";
import { removeKitAnnouncer } from "./scripts/kit-announcer.mjs";
import { directives } from "./csp.mjs";
import { sveltekit, type Config } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";
import { pwaManifest } from "./scripts/gen-manifest.mjs";
import { workspaceVersion } from "./scripts/workspace-version.mjs";

const appVersion = workspaceVersion();
const version = (process.env.PWA_VERSION ??= String(Date.now()));

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(appVersion)
  },
  plugins: [
    pwaManifest(),
    sveltekit({
      preprocess: [{ markup: removeKitAnnouncer }, vitePreprocess()],
      adapter: adapter({
        pages: process.env.PWA_BUILD_DIR ?? "build",
        assets: process.env.PWA_BUILD_DIR ?? "build",
        fallback: "200.html",
        strict: false,
      }),
      version: { name: version },
      csp: { mode: "hash", directives: directives as unknown as NonNullable<Config['csp']>['directives'] },
    }),
  ],
  server: {
    fs: { allow: [".."] },
    proxy: {
      "/api": {
        target: process.env.HUB_URL ?? "http://127.0.0.1:8080",
        ws: true,
        // Вход по cookie сеанса: прокси только подставляет разрешённый хабом Origin.
        headers: {
          Origin: process.env.HUB_ALLOWED_ORIGIN ?? "http://localhost:5173",
        },
      },
    },
  },
});
