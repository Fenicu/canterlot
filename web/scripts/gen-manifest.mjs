import { readFileSync } from 'node:fs';

export function darkTokens(css) {
  return Object.fromEntries([...css.slice(0, css.indexOf('}')).matchAll(/--([\w-]+):\s*([^;]+);/g)].map(([, key, value]) => [key, value.trim()]));
}

export function manifest(css) {
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
