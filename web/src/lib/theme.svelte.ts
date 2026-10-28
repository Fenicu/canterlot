export type ThemePref = 'dark' | 'light' | 'system';
let pref = $state<ThemePref>('dark');
let effective = $state<'dark' | 'light'>('dark');
let release: (() => void) | undefined;
function apply(): void {
  effective = pref === 'system' ? (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : pref;
  const root = document.documentElement;
  const previous = root.dataset.theme;
  const changed = Boolean(previous && previous !== effective);
  if (changed) root.dataset.themeSwitching = '';
  root.dataset.theme = effective;
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', getComputedStyle(root).getPropertyValue('--bg').trim());
  if (changed) {
    requestAnimationFrame(() => requestAnimationFrame(() => delete root.dataset.themeSwitching));
  }
}
export const theme = {
  get pref() { return pref; },
  get effective() { return effective; },
  set(value: ThemePref) {
    pref = value;
    try { localStorage.setItem('canterlot.theme', value); } catch { /* Тема остаётся доступной без хранилища. */ }
    apply();
  },
  toggle() { theme.set(effective === 'dark' ? 'light' : 'dark'); }
};
export function initTheme(): () => void {
  release?.();
  try {
    const value = localStorage.getItem('canterlot.theme');
    pref = value === 'light' || value === 'system' ? value : 'dark';
  } catch { pref = 'dark'; }
  const media = window.matchMedia('(prefers-color-scheme: light)');
  const update = () => { if (pref === 'system') apply(); };
  const storage = (event: StorageEvent) => {
    if (event.key !== 'canterlot.theme') return;
    pref = event.newValue === 'system' || event.newValue === 'light' ? event.newValue : 'dark';
    apply();
  };
  media.addEventListener('change', update);
  window.addEventListener('storage', storage);
  apply();
  release = () => { media.removeEventListener('change', update); window.removeEventListener('storage', storage); };
  return release;
}
