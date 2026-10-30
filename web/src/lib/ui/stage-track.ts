import { durationMs, prefersReducedMotion } from '../motion';
export type Stage = { id: string; label: string; kind?: 'pulse' | 'spin' | 'ring' };
export type StageState = 'active' | 'held' | 'done' | 'error';
export function swapLabel(node: HTMLElement, initial: string) {
  let target = initial;
  let leaving: Animation | undefined;
  let entering: Animation | undefined;
  node.textContent = initial;
  function finish() {
    leaving?.cancel(); entering?.cancel(); leaving = undefined; entering = undefined;
    node.textContent = target;
  }
  function update(value: string) {
    target = value;
    // Каждый durationMs пересчитывает стиль документа: идущая смена подписи сама подхватит новый target.
    if (leaving) return;
    if (!durationMs('--duration-label-out') || !node.animate) { finish(); return; }
    if (node.textContent === target) return;
    entering?.cancel();
    const style = getComputedStyle(document.documentElement);
    const shift = style.getPropertyValue('--shift-label').trim();
    const blur = style.getPropertyValue('--blur-label').trim();
    const easing = style.getPropertyValue('--ease-label').trim();
    leaving = node.animate([{ opacity: 1, transform: 'none', filter: 'blur(0)' }, { opacity: 0, transform: `translateY(-${shift})`, filter: `blur(${blur})` }], { duration: durationMs('--duration-label-out'), easing, fill: 'forwards' });
    leaving.onfinish = () => {
      node.textContent = target;
      entering = node.animate([{ opacity: 0, transform: `translateY(${shift})`, filter: `blur(${blur})` }, { opacity: 1, transform: 'none', filter: 'blur(0)' }], { duration: durationMs('--duration-label-in'), easing });
      leaving?.cancel(); leaving = undefined;
    };
  }
  const media = matchMedia('(prefers-reduced-motion: reduce)');
  const motionChange = () => { if (prefersReducedMotion()) finish(); };
  media.addEventListener('change', motionChange);
  const observer = new MutationObserver(motionChange);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'data-motion'] });
  return { update, destroy() { finish(); media.removeEventListener('change', motionChange); observer.disconnect(); } };
}
