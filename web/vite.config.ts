import adapter from "@sveltejs/adapter-static";
import { cpSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";
import { removeKitAnnouncer } from "./scripts/kit-announcer.mjs";
import { directives } from "./csp.mjs";
import { sveltekit, type Config } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";
import { pwaManifest } from "./scripts/gen-manifest.mjs";
import { workspaceVersion } from "./scripts/workspace-version.mjs";

const appVersion = workspaceVersion();
const version = (process.env.PWA_VERSION ??= String(Date.now()));

// Маршруты проверок живут вне src/routes и попадают в сборку только для UI-тестов:
// обычной сборке они не нужны, а лист весит ровно столько, сколько весит.
const testRoutes = fileURLToPath(new URL("./src/routes/__test", import.meta.url));
const testRoutesSource = fileURLToPath(new URL("./src/test-routes/__test", import.meta.url));
if (process.env.VITE_UI_TESTS === "1") {
  rmSync(testRoutes, { recursive: true, force: true });
  cpSync(testRoutesSource, testRoutes, { recursive: true });
} else {
  rmSync(testRoutes, { recursive: true, force: true });
}

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
        headers: {
          "Remote-User": process.env.HUB_OWNER ?? "owner",
          Origin: process.env.HUB_ALLOWED_ORIGIN ?? "http://localhost:5173",
        },
      },
    },
  },
});
