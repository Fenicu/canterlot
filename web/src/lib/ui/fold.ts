import type { TransitionConfig } from 'svelte/transition';
import { durationMs, easing } from '#lib/motion.js';

/**
 * Строка списка уходит или возвращается на своё место: высота сходится к нулю или растёт из него,
 * соседи едут следом, а не прыгают. Переход локальный: первая отрисовка списка и смена вкладки
 * строят строки сразу, движение получают только строки, ушедшие или пришедшие на глазах.
 */
export function rowFold(node: HTMLElement): TransitionConfig {
  haunt(node);
  const height = node.getBoundingClientRect().height;
  return {
    duration: durationMs('--duration-row-fold'),
    easing: easing('--ease-row-fold'),
    css: (t) => `overflow: clip; height: ${t * height}px; opacity: ${Math.min(1, t * 1.5)};`
  };
}

const IDENTITY = ['data-session', 'data-slot', 'data-section-slot'];
const watched = new WeakSet<HTMLElement>();

function marked(node: HTMLElement): HTMLElement[] {
  return [node, ...node.querySelectorAll<HTMLElement>(IDENTITY.map((name) => `[${name}], [${name}-leaving]`).join(', '))];
}

function leave(node: HTMLElement): void {
  node.setAttribute('aria-hidden', 'true');
  for (const el of marked(node)) for (const name of IDENTITY) {
    const value = el.getAttribute(name);
    if (value === null) continue;
    el.setAttribute(`${name}-leaving`, value);
    el.removeAttribute(name);
  }
}

function stay(node: HTMLElement): void {
  node.removeAttribute('aria-hidden');
  for (const el of marked(node)) for (const name of IDENTITY) {
    const value = el.getAttribute(`${name}-leaving`);
    if (value === null) continue;
    el.setAttribute(name, value);
    el.removeAttribute(`${name}-leaving`);
  }
}

/**
 * Уходящая строка ещё видна, пока складывается, но это уже след: диктор её не читает,
 * а поиск строки по сессии находит только живую, в том числе ту же сессию на новом месте.
 * Svelte помечает уходящий узел `inert` до расчёта перехода, поэтому след ставится в тот же такт.
 * Возврат на полпути снимает отметки обратно.
 */
function haunt(node: HTMLElement): void {
  if (node.inert) leave(node);
  if (watched.has(node)) return;
  watched.add(node);
  node.addEventListener('outrostart', () => leave(node));
  node.addEventListener('introstart', () => stay(node));
}
