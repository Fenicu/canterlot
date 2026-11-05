import { readFileSync } from 'node:fs';

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

export function manifest(css) {
  // Заставка установленного приложения совпадает с фоном палитры по умолчанию.
  const tokens = darkTokens(css);
  return {
    id: '/', start_url: '/', scope: '/', name: 'Canterlot', short_name: 'Canterlot', lang: 'ru', display: 'standalone',
    theme_color: tokens.bg, background_color: tokens.bg,
    icons: [
      { src: '/icons/icon-192.png', sizes: '192x192', type: 'image/png', purpose: 'any' },
      { src: '/icons/icon-512.png', sizes: '512x512', type: 'image/png', purpose: 'any' },
      { src: '/icons/icon-maskable-512.png', sizes: '512x512', type: 'image/png', purpose: 'maskable' }
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
