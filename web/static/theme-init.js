(() => {
  let pref = 'dark';
  try { pref = localStorage.getItem('canterlot.theme') || 'dark'; } catch { /* Хранилище может быть недоступно. */ }
  if (!['dark', 'light', 'system'].includes(pref)) pref = 'dark';
  document.documentElement.dataset.theme = pref === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : pref;
})();
