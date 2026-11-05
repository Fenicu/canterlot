(() => {
  // Список и палитра по умолчанию совпадают с src/lib/palettes.ts: это сверяет тест темы.
  const palettes = ['standard', 'aquamarine', 'vscode', 'ink', 'graphite', 'slate', 'sepia', 'terracotta', 'forest', 'midnight', 'lavender', 'strawberry', 'lemonade', 'nord', 'high-contrast'];
  const fallback = 'standard';
  let pref = 'dark';
  let palette = fallback;
  try {
    pref = localStorage.getItem('canterlot.theme') || 'dark';
    palette = localStorage.getItem('canterlot.palette') || fallback;
  } catch { /* Хранилище может быть недоступно. */ }
  if (!['dark', 'light', 'system'].includes(pref)) pref = 'dark';
  if (!palettes.includes(palette)) palette = fallback;
  document.documentElement.dataset.palette = palette;
  document.documentElement.dataset.theme = pref === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : pref;
})();
