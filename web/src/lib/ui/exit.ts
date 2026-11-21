import type { TransitionConfig } from 'svelte/transition';

/** Самый долгий из переходов элемента с задержкой, мс: список значений в вычисленном стиле читается целиком. */
function transitionSpan(node: HTMLElement): number {
  const style = getComputedStyle(node);
  const ms = (list: string) => Math.max(0, ...list.split(',').map((item) => {
    const value = parseFloat(item);
    if (!Number.isFinite(value)) return 0;
    return item.trim().endsWith('ms') ? value : value * 1000;
  }));
  return ms(style.transitionDuration) + ms(style.transitionDelay);
}

/** Уход поверхности ещё идёт: для листа это переход, начатый закрытием, для меню — переход, начатый меткой `data-leaving`. */
function drawing(node: HTMLElement): boolean {
  return node.getAnimations().some((animation) => animation instanceof CSSTransition);
}

/**
 * Уход поверхности, которую хозяин убрал из документа: `out:leave|global`.
 *
 * Сам уход рисует CSS: у меню и окна это переход по `[data-leaving]`, у листа — дискретный переход
 * `display` и `overlay` после `dialog.close()`. Здесь поверхность только остаётся в документе на время
 * перехода, чтобы Svelte не снял узел раньше, чем уход дорисован. Поэтому закрывается всё сразу:
 * `before` делает то, что должно случиться в первый кадр (лист закрывается, возвращает фокус и снимает
 * блокировку страницы), а уходящий узел ничего не принимает: `inert` и скрыт для чтения с экрана.
 * Длительность берётся из вычисленного `transition-duration`, значит, масштаб движения и reduced motion
 * (0 мс) работают без своего кода; где перехода нет, узел снимается сразу.
 *
 * `before` может вернуть `false`, если закрыто давно и рисовать нечего.
 */
export function leave(node: HTMLElement, params: { before?: () => boolean | void } = {}): TransitionConfig {
  const idle = params.before?.() === false && !drawing(node);
  if (idle) return { duration: 0 };
  node.setAttribute('data-leaving', '');
  node.setAttribute('aria-hidden', 'true');
  node.inert = true;
  return { duration: transitionSpan(node) };
}
