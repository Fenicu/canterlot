import { expect, test } from 'vitest';
import { existsSync, readFileSync } from 'node:fs';

// Эти проверки читают production-сборку, поэтому check-web.sh запускает их
// отдельным прогоном после сборок, а не параллельно с ними.
test('built_manifest_matches_the_generated_one', async (ctx) => {
  if (!existsSync('build/manifest.webmanifest')) ctx.skip('нет build/manifest.webmanifest: production-сборка не запускалась');
  const { manifest } = await import('../../scripts/gen-manifest.mjs');
  const css = readFileSync('src/lib/styles/tokens.css', 'utf8');
  expect(JSON.parse(readFileSync('build/manifest.webmanifest', 'utf8'))).toEqual(manifest(css));
});

test('built_shell_has_only_hashed_or_external_scripts', async (ctx) => {
  if (!existsSync('build/200.html')) ctx.skip('нет build/200.html: production-сборка не запускалась');
  const { scriptHashes, scriptsAllowed } = await import('../../src/lib/pwa/csp');
  const built = readFileSync('build/200.html', 'utf8');
  const hashes = await scriptHashes(built);
  expect(hashes.length).toBeGreaterThan(0);
  expect(await scriptsAllowed(built, hashes)).toBe(true);
  for (const hash of hashes) expect(built).toContain(`'${hash}'`);
  expect(built).toContain('<script src="/theme-init.js"></script>');
  const bootstrapId = built.match(/(__sveltekit_\w+) =/)?.[1];
  const start = built.match(/import\("([^\"]*entry\/start[^\"]+)"\)/)?.[1];
  expect(bootstrapId).toBeTruthy(); expect(start).toBeTruthy();
  expect(built).toContain(`kit.init(${bootstrapId})`);
});
