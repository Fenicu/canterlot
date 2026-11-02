/// <reference lib="webworker" />
const sw = self as unknown as ServiceWorkerGlobalScope;

// Аварийный worker для устройств, где ещё установлен старый service worker.
// Удаляем после того, как им перестанут приходить обновления (см. docs/design/pwa.md).
sw.addEventListener('install', event => {
  event.waitUntil(sw.skipWaiting());
});

sw.addEventListener('activate', event => {
  event.waitUntil((async () => {
    for (const key of await caches.keys()) await caches.delete(key);
    await sw.registration.unregister();
    await sw.clients.claim();
    const windows = await sw.clients.matchAll({ type: 'window', includeUncontrolled: true });
    await Promise.all(windows.map(client => client.navigate(client.url).catch(() => undefined)));
  })());
});
