import { durationMs } from '../motion';

export interface SwapWord {
  /** Слово как есть; пустое убирает элемент из документа. */
  text: string;
  /** Состояние, о котором слово. Движение только при смене ключа: слово при том же состоянии подменяется сразу. */
  key: string;
}

/**
 * Слово состояния («идёт» → «2 записи»): прежнее уходит вверх, новое приходит снизу, как подпись этапа
 * в StageTrack. Действие Svelte: `<span use:swapOn={{ text, key }}></span>`.
 *
 * - Текст пишется в тот же текстовый узел: лента следит, чтобы тикающие подписи не пересоздавали узлы.
 * - Тикающий срок («12 мин» → «13 мин») держит тот же `key`: он подменяется на месте и не двигается.
 * - Опустевшее слово уходит из документа вместе с элементом, когда допроигран уход. На его месте
 *   остаётся пустой комментарий, а при возвращении слова элемент встаёт обратно. Поэтому элемент
 *   должен лежать внутри родительского элемента, а не быть единственным корнем блока шаблона.
 * - При выключенном движении (система или масштаб 0) анимаций нет: слово меняется и убирается сразу.
 */
export function swapOn(node: HTMLElement, initial: SwapWord) {
  let target = initial.text;
  let key = initial.key;
  let leaving: Animation | undefined;
  let entering: Animation | undefined;
  let hole: Comment | undefined;

  /** Текст пишется в существующий текстовый узел, а не заменяет его. */
  function write(text: string) {
    const first = node.firstChild;
    if (first instanceof Text && node.childNodes.length === 1) {
      if (first.data !== text) first.data = text;
    } else node.textContent = text;
  }
  function vanish() {
    if (hole) return;
    hole = document.createComment('');
    node.replaceWith(hole);
  }
  function appear() {
    if (!hole) return;
    hole.replaceWith(node);
    hole = undefined;
  }
  function settle() {
    if (target === '') {
      write('');
      vanish();
    } else {
      appear();
      write(target);
    }
  }
  function style() {
    const root = getComputedStyle(document.documentElement);
    return { shift: root.getPropertyValue('--shift-label').trim(), blur: root.getPropertyValue('--blur-label').trim(), easing: root.getPropertyValue('--ease-label').trim() };
  }
  function arrive() {
    if (!node.animate) return;
    const { shift, blur, easing } = style();
    entering = node.animate([{ opacity: 0, transform: `translateY(${shift})`, filter: `blur(${blur})` }, { opacity: 1, transform: 'none', filter: 'blur(0)' }], { duration: durationMs('--duration-label-in'), easing });
  }

  settle();

  function update(next: SwapWord) {
    target = next.text;
    const changed = next.key !== key;
    key = next.key;
    // Идущий уход сам подхватит новое слово.
    if (leaving) return;
    const moves = changed && durationMs('--duration-label-out') > 0 && typeof node.animate === 'function';
    if (!moves) {
      entering?.cancel();
      entering = undefined;
      settle();
      return;
    }
    entering?.cancel();
    entering = undefined;
    if (hole) {
      // Слова не было: оно только приходит.
      settle();
      arrive();
      return;
    }
    const { shift, blur, easing } = style();
    leaving = node.animate([{ opacity: 1, transform: 'none', filter: 'blur(0)' }, { opacity: 0, transform: `translateY(-${shift})`, filter: `blur(${blur})` }], { duration: durationMs('--duration-label-out'), easing, fill: 'forwards' });
    leaving.onfinish = () => {
      const gone = target === '';
      leaving?.cancel();
      leaving = undefined;
      settle();
      if (!gone) arrive();
    };
  }

  return {
    update,
    destroy() {
      leaving?.cancel();
      entering?.cancel();
      leaving = undefined;
      entering = undefined;
      hole?.remove();
      hole = undefined;
    }
  };
}
