// Поведение старого worker из предыдущей сборки: управляет страницей и держит оболочку
// в кэше `canterlot-shell-legacy`. Нужен только для проверки аварийного перехода.
self.addEventListener('install', event => {
  event.waitUntil(caches.open('canterlot-shell-legacy').then(cache => cache.put('/legacy-shell', new Response('old shell'))));
});

self.addEventListener('activate', event => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', () => {
  // Сеть без подмены: миграция проверяет снятие worker, а не его офлайн-оболочку.
});
