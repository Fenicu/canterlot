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
    css: (t) => `overflow: clip; height: ${(t * height).toFixed(3)}px; opacity: ${Math.min(1, t * 1.5).toFixed(3)};`
  };
}

/**
 * Новая ветка дерева делегирования вырастает из своей связи: место раскрывается по высоте,
 * а строка выходит от ствола «└» вправо.
 */
export function branchGrow(node: HTMLElement): TransitionConfig {
  const height = node.getBoundingClientRect().height;
  const shift = parseFloat(getComputedStyle(node).getPropertyValue('--shift-enter')) || 0;
  return {
    duration: durationMs('--duration-branch-grow'),
    easing: easing('--ease-branch-grow'),
    css: (t) => `overflow: clip; height: ${(t * height).toFixed(3)}px; opacity: ${Math.min(1, t * 1.5).toFixed(3)}; transform: translateX(${((t - 1) * shift).toFixed(3)}px);`
  };
}

/**
 * Тело блока ленты раскрывается и сворачивается сеткой: строка сетки идёт от 0fr к 1fr, содержимое
 * не перестраивается, а лента меряет растущую строку каждый кадр и держит читаемое на месте.
 * Ключевые кадры считаются здесь, поэтому движение плавное и там, где браузер не интерполирует
 * строки сетки сам. Переход локальный: строка, вернувшаяся в окно ленты раскрытой, не раскрывается заново.
 */
export const blockFold: (node: HTMLElement) => TransitionConfig = () => ({
  duration: durationMs('--duration-expand'),
  easing: easing('--ease-expand'),
  css: (t) => `grid-template-rows: ${t.toFixed(4)}fr; opacity: ${Math.min(1, t * 2).toFixed(3)};`
});

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
