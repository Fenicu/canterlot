import { durationMs, easing, fadeMs, prefersReducedMotion } from '../motion';

/**
 * Появление плавающей карточки (баннер связи, тост): короткий сдвиг на токен `--shift-enter` и прозрачность.
 * Без движения остаётся только прозрачность. Переход пишет `transform` самой карточки: расставлять её
 * тем же `transform` нельзя, место задаёт внешний слой.
 */
export function arrive(node: HTMLElement) {
  if (prefersReducedMotion()) return { duration: fadeMs('--duration-fast'), css: (t: number) => `opacity: ${t}` };
  const shift = parseFloat(getComputedStyle(node).getPropertyValue('--shift-enter')) || 0;
  return {
    duration: durationMs('--duration-enter'),
    easing: easing('--ease-enter'),
    css: (t: number) => `opacity: ${t}; transform: translateY(${(t - 1) * shift}px)`
  };
}

/**
 * Уход той же карточки: быстрее прихода и по кривой выхода, чтобы она не задерживалась на экране.
 * Сдвиг вниз к месту, откуда пришла; без движения только прозрачность.
 */
export function leave(node: HTMLElement) {
  if (prefersReducedMotion()) return { duration: fadeMs('--duration-fast'), css: (t: number) => `opacity: ${t}` };
  const shift = parseFloat(getComputedStyle(node).getPropertyValue('--shift-enter')) || 0;
  return {
    duration: durationMs('--duration-fast'),
    easing: easing('--ease-exit'),
    css: (t: number) => `opacity: ${t}; transform: translateY(${(t - 1) * shift}px)`
  };
}
