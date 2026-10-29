/// <reference lib="webworker" />
import { build, files, version } from '$service-worker';
import { cacheable, classify, navigationResponse } from './lib/pwa/sw-routing';
import { openDb } from './lib/storage/idb';
import { stashShare } from './lib/pwa/share';

const sw = self as unknown as ServiceWorkerGlobalScope;
const cacheName = `canterlot-shell-${version}`;
const assets = new Set([...build, ...files, '/manifest.webmanifest', '/200.html']);

sw.addEventListener('install', event => {
  event.waitUntil((async () => {
    const cache = await caches.open(cacheName);
    for (const path of assets) {
      const response = await fetch(new Request(path, { credentials: 'include', redirect: 'manual', cache: 'reload' }));
      if (!cacheable(response)) throw new Error('precache_rejected');
      const type = response.headers.get('Content-Type') ?? '';
      if (path !== '/200.html' && type.includes('text/html')) throw new Error('precache_auth_html');
      await cache.put(path, response);
    }
  })());
});

sw.addEventListener('activate', event => {
  event.waitUntil((async () => {
    for (const key of await caches.keys()) if (key.startsWith('canterlot-shell-') && key !== cacheName) await caches.delete(key);
    await sw.clients.claim();
  })());
});

sw.addEventListener('message', event => {
  if (event.data?.type === 'SKIP_WAITING') event.waitUntil(sw.skipWaiting());
});

sw.addEventListener('fetch', event => {
  const req = event.request;
  if (new URL(req.url).origin !== sw.location.origin) return;
  const route = classify(req);
  if (route === 'share_target') {
    event.respondWith((async () => {
      const system = await openDb('__system__');
      const owner = await system.get<string>('kv', 'current_owner');
      if (!owner) return Response.redirect(new URL('/share?error=sw_inactive', sw.location.origin), 303);
      const id = await stashShare(await openDb(owner), await req.formData());
      return Response.redirect(new URL(`/share?id=${encodeURIComponent(id)}`, sw.location.origin), 303);
    })());
  } else if (route === 'navigation') {
    event.respondWith(navigationResponse(req, () => fetch(req), async () => (await caches.open(cacheName)).match('/200.html')));
  } else if (route === 'asset' && assets.has(new URL(req.url).pathname)) {
    event.respondWith((async () => (await (await caches.open(cacheName)).match(req)) ?? fetch(req))());
  }
});
