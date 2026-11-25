import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import postcss from 'postcss';

/**
 * Цвета тёмного варианта палитры. Без имени — палитра по умолчанию: её тёмный вариант
 * стоит в `:root` рядом со своим селектором, голый `:root` держит только нецветовые токены.
 */
export function darkTokens(css, palette) {
  const tokens = {};
  postcss.parse(css).walkRules(rule => {
    if (rule.parent?.type !== 'root') return;
    const selectors = rule.selectors.map(selector => selector.trim());
    const match = palette ? selectors.includes(`[data-palette=${palette}][data-theme=dark]`) : selectors.includes(':root') && selectors.some(selector => selector.endsWith('[data-theme=dark]'));
    if (match) rule.walkDecls(decl => { if (decl.prop.startsWith('--')) tokens[decl.prop.slice(2)] = decl.value.trim(); });
  });
  return tokens;
}

/**
 * Адрес значка с хэшем содержимого: прежний адрес Chrome держал в кеше, и установка
 * показывала значок старой версии. Новый файл — новый адрес.
 */
function versioned(src) {
  // Путь собирается без new URL(…, import.meta.url): Vite переписывает такой шаблон в адрес ассета.
  const body = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', 'static', src));
  return `${src}?v=${createHash('sha256').update(body).digest('hex').slice(0, 12)}`;
}

export function manifest(css) {
  // Заставка установленного приложения совпадает с фоном палитры по умолчанию.
  const tokens = darkTokens(css);
  return {
    id: '/', start_url: '/', scope: '/', name: 'Canterlot', short_name: 'Canterlot', lang: 'ru', display: 'standalone',
    theme_color: tokens.bg, background_color: tokens.bg,
    icons: [
      { src: versioned('/icons/icon-192.png'), sizes: '192x192', type: 'image/png', purpose: 'any' },
      { src: versioned('/icons/icon-512.png'), sizes: '512x512', type: 'image/png', purpose: 'any' },
      { src: versioned('/icons/icon-maskable-512.png'), sizes: '512x512', type: 'image/png', purpose: 'maskable' }
    ],
    shortcuts: [{ name: 'Новая сессия', url: '/new' }]
  };
}

export function pwaManifest() {
  const generate = () => JSON.stringify(manifest(readFileSync(new URL('../src/lib/styles/tokens.css', import.meta.url), 'utf8')), null, 2);
  return {
    name: 'canterlot-manifest',
    generateBundle() { this.emitFile({ type: 'asset', fileName: 'manifest.webmanifest', source: generate() }); },
    configureServer(server) {
      server.middlewares.use('/manifest.webmanifest', (_req, res) => { res.setHeader('Content-Type', 'application/manifest+json'); res.end(generate()); });
    }
  };
}
