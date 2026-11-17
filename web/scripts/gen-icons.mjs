import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { chromium } from '@playwright/test';

const CANVAS = 512;
const CENTER = CANVAS / 2;
/** Безопасная зона maskable-значка: круг радиусом 40% стороны. */
export const MASKABLE_SAFE_RADIUS = CANVAS * 0.4;
/** Замок уменьшен до 75%: верхний шпиль и нижние зубцы остаются в круге с запасом. */
export const MASKABLE_SCALE = 0.75;
const BACKGROUND = /<rect width="512" height="512"(?: rx="\d+")? fill="#[0-9a-f]{6}"\/>/;

/** Значок фирменный и не зависит от палитры: у источника только его собственные цвета. */
export function withoutTokens(svg) {
  if (svg.includes('{{')) throw new Error('Icon source still contains tokens');
  return svg;
}

/** Квадратный фон без скругления: iOS сам скругляет apple-touch-icon, а maskable не любит прозрачность. */
export function fullBleed(svg) {
  const source = withoutTokens(svg);
  if (!BACKGROUND.test(source)) throw new Error('Icon source has no 512×512 background');
  return source.replace(BACKGROUND, (rect) => rect.replace(/ rx="\d+"/, ''));
}

/** Квадратный фон и замок, вписанный в безопасный круг maskable-значка. */
export function maskable(svg) {
  const source = fullBleed(svg);
  const rect = source.match(BACKGROUND)?.[0];
  if (!rect) throw new Error('Icon source has no 512×512 background');
  const [before, after] = source.split(rect);
  const body = after.replace(/\s*<\/svg>\s*$/, '');
  return `${before}${rect}<g transform="translate(${CENTER} ${CENTER}) scale(${MASKABLE_SCALE}) translate(-${CENTER} -${CENTER})">${body}</g></svg>`;
}

/** Что пишем из одного источника: SVG-фавикон, PNG-фолбэк и значки установки. */
export function iconVariants(svg) {
  const rounded = withoutTokens(svg);
  const square = fullBleed(svg);
  return [
    { name: 'favicon.svg', svg: rounded },
    { name: 'favicon-32.png', size: 32, svg: rounded, transparent: true },
    { name: 'apple-touch-icon.png', size: 180, svg: square, transparent: false },
    { name: 'icon-192.png', size: 192, svg: rounded, transparent: true },
    { name: 'icon-512.png', size: 512, svg: rounded, transparent: true },
    { name: 'icon-maskable-512.png', size: 512, svg: maskable(svg), transparent: false }
  ];
}

export async function generateIcons() {
  const dir = new URL('../static/icons/', import.meta.url);
  const variants = iconVariants(await readFile(new URL('source.svg', dir), 'utf8'));
  await mkdir(dir, { recursive: true });
  await writeFile(new URL('favicon.svg', dir), variants[0].svg);
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({ deviceScaleFactor: 1 });
    for (const icon of variants) {
      if (!icon.size) continue;
      await page.setViewportSize({ width: icon.size, height: icon.size });
      const scaled = icon.svg.replace('<svg ', `<svg width="100%" height="100%" `);
      await page.goto(`data:image/svg+xml,${encodeURIComponent(scaled)}`);
      await writeFile(new URL(icon.name, dir), await page.screenshot({ omitBackground: icon.transparent }));
    }
  } finally { await browser.close(); }
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await generateIcons();
