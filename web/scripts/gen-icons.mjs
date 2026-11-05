import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { chromium } from '@playwright/test';
import { darkTokens } from './gen-manifest.mjs';

// Значок фирменный и не зависит от палитры по умолчанию: он всегда в тёмном Аквамарине.
export function colouredSvg(source, css) {
  const tokens = darkTokens(css, 'aquamarine');
  return source.replace(/\{\{([\w-]+)\}\}/g, (_, name) => {
    if (!tokens[name]) throw new Error(`Missing icon token: ${name}`);
    return tokens[name];
  });
}

export async function generateIcons() {
  const dir = new URL('../static/icons/', import.meta.url);
  const svg = colouredSvg(await readFile(new URL('source.svg', dir), 'utf8'), await readFile(new URL('../src/lib/styles/tokens.css', import.meta.url), 'utf8'));
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({ deviceScaleFactor: 1 });
    for (const [name, size] of [['icon-192', 192], ['icon-512', 512], ['icon-maskable-512', 512]]) {
      await page.setViewportSize({ width: size, height: size });
      await page.goto(`data:image/svg+xml,${encodeURIComponent(svg)}`);
      await mkdir(dir, { recursive: true });
      await writeFile(new URL(`${name}.png`, dir), await page.screenshot({ omitBackground: false }));
    }
  } finally { await browser.close(); }
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await generateIcons();
