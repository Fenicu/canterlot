import { flushSync } from 'svelte';
import { DEFAULT_PALETTE, paletteOrDefault, type PalettePref } from './palettes';
import { durationMs, prefersReducedMotion } from './motion';
export type ThemePref = 'dark' | 'light' | 'system';
export type { PalettePref } from './palettes';
let pref = $state<ThemePref>('dark');
let palette = $state<PalettePref>(DEFAULT_PALETTE);
let effective = $state<'dark' | 'light'>('dark');
let release: (() => void) | undefined;
let pointer: { x: number; y: number; at: number } | null = null;
let generation = 0;
// Нажатие старше этого уже не считается причиной смены темы: круг тогда растёт от фокуса.
const POINTER_FRESH_MS = 1500;

function commit(): void {
  const root = document.documentElement;
  root.dataset.palette = palette;
  root.dataset.theme = effective;
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', getComputedStyle(root).getPropertyValue('--bg').trim());
}
/** Точка, из которой раскрывается новая тема: место нажатия, иначе центр элемента в фокусе, иначе середина окна. */
function origin(): { x: number; y: number } {
  if (pointer && performance.now() - pointer.at < POINTER_FRESH_MS) return pointer;
  const active = document.activeElement;
  if (active instanceof HTMLElement && active !== document.body) {
    const box = (active.closest('label') ?? active).getBoundingClientRect();
    if (box.width || box.height) return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
  }
  return { x: window.innerWidth / 2, y: window.innerHeight / 2 };
}
/**
 * Меняет тему. Явный выбор владельца раскрывает новую тему кругом через View Transitions;
 * синхронизация вкладок, смена темы ОС, отключённое движение и браузеры без API переключают сразу.
 * Переходы цвета у элементов на время смены выключены: новая тема не доезжает до себя по частям.
 */
function resolveMode(value: ThemePref): 'dark' | 'light' {
  return value === 'system' ? (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : value;
}
function apply(nextPref: ThemePref = pref, nextPalette: PalettePref = palette, animate = false): void {
  const nextEffective = resolveMode(nextPref);
  const root = document.documentElement;
  // Состояние меняется вместе с атрибутами: снимок старой темы не должен успеть получить новую разметку.
  const assign = () => { pref = nextPref; palette = nextPalette; effective = nextEffective; };
  const changed = Boolean(root.dataset.theme && (root.dataset.theme !== nextEffective || root.dataset.palette !== nextPalette));
  if (!changed) { assign(); commit(); return; }
  const id = ++generation;
  const settle = () => { if (id === generation) { delete root.dataset.themeSwitching; delete root.dataset.themeReveal; } };
  const reveal = animate && typeof document.startViewTransition === 'function' && document.visibilityState === 'visible' && !prefersReducedMotion();
  if (!reveal) {
    delete root.dataset.themeReveal;
    root.dataset.themeSwitching = '';
    assign();
    commit();
    requestAnimationFrame(() => requestAnimationFrame(settle));
    return;
  }
  const point = origin();
  root.dataset.themeReveal = '';
  const transition = document.startViewTransition(() => { root.dataset.themeSwitching = ''; assign(); flushSync(); commit(); });
  transition.ready.then(() => {
    if (typeof root.animate !== 'function') return;
    const radius = Math.hypot(Math.max(point.x, window.innerWidth - point.x), Math.max(point.y, window.innerHeight - point.y));
    const at = `at ${point.x}px ${point.y}px`;
    root.animate({ clipPath: [`circle(0 ${at})`, `circle(${radius}px ${at})`] }, {
      duration: durationMs('--duration-theme-reveal'),
      easing: getComputedStyle(root).getPropertyValue('--ease-theme-reveal').trim() || 'linear',
      pseudoElement: '::view-transition-new(root)'
    });
  }).catch(() => { /* Переход пропущен: тема уже применена. */ });
  transition.finished.finally(settle).catch(() => { /* Ошибка перехода не мешает теме. */ });
}
export const theme = {
  get pref() { return pref; },
  get effective() { return effective; },
  get palette() { return palette; },
  set(value: ThemePref) {
    try { localStorage.setItem('canterlot.theme', value); } catch { /* Тема остаётся доступной без хранилища. */ }
    apply(value, palette, true);
  },
  setPalette(value: PalettePref) {
    const next = paletteOrDefault(value);
    try { localStorage.setItem('canterlot.palette', next); } catch { /* Палитра остаётся доступной без хранилища. */ }
    apply(pref, next, true);
  },
  toggle() { theme.set(effective === 'dark' ? 'light' : 'dark'); }
};
export function initTheme(): () => void {
  release?.();
  let storedPref: ThemePref = 'dark';
  let storedPalette: PalettePref = DEFAULT_PALETTE;
  try {
    const value = localStorage.getItem('canterlot.theme');
    storedPref = value === 'light' || value === 'system' ? value : 'dark';
    storedPalette = paletteOrDefault(localStorage.getItem('canterlot.palette'));
  } catch { /* Без хранилища остаются тёмный режим и палитра по умолчанию. */ }
  const media = window.matchMedia('(prefers-color-scheme: light)');
  const update = () => { if (pref === 'system') apply(); };
  const storage = (event: StorageEvent) => {
    if (event.key === 'canterlot.theme') apply(event.newValue === 'system' || event.newValue === 'light' ? event.newValue : 'dark', palette);
    else if (event.key === 'canterlot.palette') apply(pref, paletteOrDefault(event.newValue));
  };
  const press = (event: PointerEvent) => { pointer = { x: event.clientX, y: event.clientY, at: performance.now() }; };
  // С клавиатуры круг растёт от элемента в фокусе, а не от прошлого нажатия.
  const key = () => { pointer = null; };
  media.addEventListener('change', update);
  window.addEventListener('storage', storage);
  window.addEventListener('pointerdown', press, { capture: true, passive: true });
  window.addEventListener('keydown', key, { capture: true, passive: true });
  apply(storedPref, storedPalette);
  release = () => {
    media.removeEventListener('change', update);
    window.removeEventListener('storage', storage);
    window.removeEventListener('pointerdown', press, { capture: true });
    window.removeEventListener('keydown', key, { capture: true });
  };
  return release;
}
