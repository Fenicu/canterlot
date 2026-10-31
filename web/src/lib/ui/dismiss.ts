export type DismissReason = 'outside' | 'escape';

export interface DismissOptions {
  onclose: (reason: DismissReason) => void;
  /** Нажатие, которое окно не закрывает: свой переключатель сам решает, что делать. */
  keep?: (target: Element) => boolean;
}

/** Всплывающее окно закрывается нажатием снаружи и Escape; модальный лист над ним свои нажатия оставляет себе. */
export function dismissable(node: HTMLElement, options: DismissOptions) {
  let current = options;
  const press = (event: Event): void => {
    const { target } = event;
    if (!(target instanceof Element) || node.contains(target)) return;
    const modal = target.closest('dialog');
    if ((modal && !modal.contains(node)) || current.keep?.(target)) return;
    current.onclose('outside');
  };
  const key = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape' || event.defaultPrevented) return;
    const modal = document.querySelector('dialog[open]');
    if (modal && !modal.contains(node)) return;
    current.onclose('escape');
  };
  window.addEventListener('pointerdown', press, true);
  // Firefox отдаёт правую кнопку только контекстным меню.
  window.addEventListener('contextmenu', press, true);
  window.addEventListener('keydown', key);
  return {
    update(next: DismissOptions): void {
      current = next;
    },
    destroy(): void {
      window.removeEventListener('pointerdown', press, true);
      window.removeEventListener('contextmenu', press, true);
      window.removeEventListener('keydown', key);
    }
  };
}
