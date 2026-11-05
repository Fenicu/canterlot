/** Ниже этой высоты поле ввода переходит в компактный вид: клавиатура уже забрала место. */
export const COMPACT_HEIGHT = 760;
/** Меньшая разница — это адресная строка или масштаб, а не экранная клавиатура. */
const KEYBOARD_MIN = 120;

/**
 * Насколько клавиатура перекрывает раскладку. При `interactive-widget=resizes-content`
 * раскладка уже сжата и смещение равно нулю; в standalone-режиме Android клавиатура
 * иногда рисуется поверх, и тогда остаётся только VisualViewport.
 */
export function keyboardInset(innerHeight: number, viewHeight: number, offsetTop: number, scale = 1): number {
  if (![innerHeight, viewHeight, offsetTop, scale].every(Number.isFinite)) return 0;
  // Масштабирование пальцами уменьшает VisualViewport, но это не клавиатура.
  if (scale > 1.01) return 0;
  return Math.max(0, Math.round(innerHeight - viewHeight - offsetTop));
}

export function compactViewport(viewHeight: number, inset: number): boolean {
  return viewHeight < COMPACT_HEIGHT || inset >= KEYBOARD_MIN;
}

/**
 * Держит `--app-height`, `--keyboard-inset` и `data-compact` на корне документа.
 * Раскладка и поле ввода читают их вместо одного `100dvh`, который не учитывает
 * клавиатуру в standalone-режиме.
 */
export function trackViewport(): () => void {
  if (typeof window === 'undefined' || typeof document === 'undefined') return () => undefined;
  const root = document.documentElement;
  let frame = 0;

  const apply = () => {
    frame = 0;
    const view = window.visualViewport;
    const height = Math.round(view?.height ?? window.innerHeight);
    const offsetTop = view?.offsetTop ?? 0;
    const scale = view?.scale ?? 1;
    const inset = keyboardInset(window.innerHeight, height, offsetTop, scale);
    root.style.setProperty('--app-height', `${height}px`);
    root.style.setProperty('--keyboard-inset', `${inset >= KEYBOARD_MIN ? inset : 0}px`);
    if (compactViewport(height, inset)) root.dataset.compact = '';
    else delete root.dataset.compact;
  };

  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(apply);
  };

  apply();
  window.addEventListener('resize', schedule);
  window.addEventListener('orientationchange', schedule);
  const view = window.visualViewport;
  view?.addEventListener('resize', schedule);
  view?.addEventListener('scroll', schedule);

  return () => {
    if (frame) cancelAnimationFrame(frame);
    window.removeEventListener('resize', schedule);
    window.removeEventListener('orientationchange', schedule);
    view?.removeEventListener('resize', schedule);
    view?.removeEventListener('scroll', schedule);
    root.style.removeProperty('--app-height');
    root.style.removeProperty('--keyboard-inset');
    delete root.dataset.compact;
  };
}
