(() => {
  let pref = 'dark';
  let palette = 'aquamarine';
  try {
    pref = localStorage.getItem('canterlot.theme') || 'dark';
    palette = localStorage.getItem('canterlot.palette') || 'aquamarine';
  } catch { /* Хранилище может быть недоступно. */ }
  if (!['dark', 'light', 'system'].includes(pref)) pref = 'dark';
  if (!['aquamarine', 'standard', 'vscode'].includes(palette)) palette = 'aquamarine';
  document.documentElement.dataset.palette = palette;
  document.documentElement.dataset.theme = pref === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : pref;
})();
