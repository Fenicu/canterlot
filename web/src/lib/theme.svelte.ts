export type ThemePref = 'dark' | 'light' | 'system';
export type PalettePref = 'aquamarine' | 'standard' | 'vscode';
const PALETTES: PalettePref[] = ['aquamarine', 'standard', 'vscode'];
let pref = $state<ThemePref>('dark');
let palette = $state<PalettePref>('aquamarine');
let effective = $state<'dark' | 'light'>('dark');
let release: (() => void) | undefined;
function apply(): void {
  effective = pref === 'system' ? (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : pref;
  const root = document.documentElement;
  const changed = Boolean(root.dataset.theme && (root.dataset.theme !== effective || root.dataset.palette !== palette));
  if (changed) root.dataset.themeSwitching = '';
  root.dataset.palette = palette;
  root.dataset.theme = effective;
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', getComputedStyle(root).getPropertyValue('--bg').trim());
  if (changed) {
    requestAnimationFrame(() => requestAnimationFrame(() => delete root.dataset.themeSwitching));
  }
}
export const theme = {
  get pref() { return pref; },
  get effective() { return effective; },
  get palette() { return palette; },
  set(value: ThemePref) {
    pref = value;
    try { localStorage.setItem('canterlot.theme', value); } catch { /* Тема остаётся доступной без хранилища. */ }
    apply();
  },
  setPalette(value: PalettePref) {
    palette = PALETTES.includes(value) ? value : 'aquamarine';
    try { localStorage.setItem('canterlot.palette', palette); } catch { /* Палитра остаётся доступной без хранилища. */ }
    apply();
  },
  toggle() { theme.set(effective === 'dark' ? 'light' : 'dark'); }
};
export function initTheme(): () => void {
  release?.();
  try {
    const value = localStorage.getItem('canterlot.theme');
    pref = value === 'light' || value === 'system' ? value : 'dark';
    const storedPalette = localStorage.getItem('canterlot.palette');
    palette = PALETTES.includes(storedPalette as PalettePref) ? storedPalette as PalettePref : 'aquamarine';
  } catch {
    pref = 'dark';
    palette = 'aquamarine';
  }
  const media = window.matchMedia('(prefers-color-scheme: light)');
  const update = () => { if (pref === 'system') apply(); };
  const storage = (event: StorageEvent) => {
    if (event.key === 'canterlot.theme') {
      pref = event.newValue === 'system' || event.newValue === 'light' ? event.newValue : 'dark';
    } else if (event.key === 'canterlot.palette') {
      palette = PALETTES.includes(event.newValue as PalettePref) ? event.newValue as PalettePref : 'aquamarine';
    } else return;
    apply();
  };
  media.addEventListener('change', update);
  window.addEventListener('storage', storage);
  apply();
  release = () => { media.removeEventListener('change', update); window.removeEventListener('storage', storage); };
  return release;
}
