import type { TransitionConfig } from 'svelte/transition';

import { durationMs } from '#lib/motion.js';

/**
 * У узла есть переход с ненулевой длительностью: CSS уход рисует. Список значений в вычисленном стиле читается
 * целиком; без движения токен 0 мс, и перехода нет, а где CSS его не объявил (Firefox без `overlay`), рисовать нечего.
 */
function drawsExit(node: HTMLElement): boolean {
  const style = getComputedStyle(node);
  const positive = (list: string) => list.split(',').some((item) => parseFloat(item) > 0);
  return positive(style.transitionDuration) || positive(style.transitionDelay);
}

/** Уход поверхности ещё идёт: для листа это переход, начатый закрытием, для меню — переход, начатый меткой `data-leaving`. */
function drawing(node: HTMLElement): boolean {
  return node.getAnimations().some((animation) => animation instanceof CSSTransition);
}

/** Анимация самого Svelte (под неё он держит узел): у CSS-переходов и CSS-анимаций класс свой. */
function held(node: HTMLElement): boolean {
  return node.getAnimations().some((animation) => !(animation instanceof CSSTransition) && !(animation instanceof CSSAnimation));
}

interface LeaveParams {
  /** Что должно случиться в первый кадр ухода. Вернуть `false`, если закрыто давно и рисовать нечего. */
  before?: () => boolean | void;
  /** Хозяин вернул поверхность, пока она уходила: Svelte оживляет тот же узел, и ему надо вернуть вид и состояние. */
  revive?: () => void;
}

/**
 * Уход поверхности, которую хозяин убрал из документа: `out:leave|global`.
 *
 * Сам уход рисует CSS: у меню и окна это переход по `[data-leaving]`, у листа — дискретный переход
 * `display` и `overlay` после `dialog.close()`. Здесь поверхность только остаётся в документе на время
 * перехода, чтобы Svelte не снял узел раньше, чем уход дорисован. Поэтому закрывается всё сразу:
 * `before` делает то, что должно случиться в первый кадр (лист закрывается, возвращает фокус и снимает
 * блокировку страницы), а уходящий узел ничего не принимает: `inert` и скрыт для чтения с экрана.
 * Узел держится `--duration-fast`: столько длится уход у каждой поверхности (CSS читает тот же токен), масштаб
 * движения и reduced motion (0 мс) его снимают; где перехода нет, узел снимается сразу.
 *
 * Если хозяин вернул поверхность в разгар ухода (меню нажато повторно), Svelte не создаёт новый узел, а
 * оживляет старый и зовёт `tick(1)`: метки снимаются, а лист открывается снова через `revive`.
 * Возвращается функция, а не готовая настройка: Svelte берёт настройку один раз на узел, а второй
 * уход того же оживлённого узла должен снова закрыть лист и поставить метки.
 */
export function leave(node: HTMLElement, params: LeaveParams = {}): () => TransitionConfig {
  return () => {
    const idle = params.before?.() === false && !drawing(node);
    if (idle) return {};
    node.setAttribute('data-leaving', '');
    node.setAttribute('aria-hidden', 'true');
    node.inert = true;
    // Без перехода узел снимается сразу (Svelte видит нулевую длительность).
    if (!drawsExit(node)) return {};
    return {
      duration: durationMs('--duration-fast'),
      tick: (t) => {
        // Первый кадр самого ухода тоже даёт t = 1, но тогда анимация Svelte ещё идёт; при оживлении её уже сняли.
        if (t !== 1 || held(node)) return;
        node.removeAttribute('data-leaving');
        node.removeAttribute('aria-hidden');
        // Оживляет блок `{#if}` в своём эффекте, где менять состояние нельзя: хозяин получает слово следом.
        queueMicrotask(() => params.revive?.());
      }
    };
  };
}
