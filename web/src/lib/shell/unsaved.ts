/**
 * Текст, который владелец набрал в формах без черновика: редакторы инструкций, расписаний, памяти, маршрутизации,
 * новая сессия, ответы на карточках. Он живёт только на экране, и перезагрузка страницы его стирает.
 *
 * Учёт общий, по событию `input`, а не пометкой в каждой форме: новая форма защищена без правок. Поле ввода чата
 * сюда не входит (его текст хранит черновик), как и поиск со счётчиками и выключателями: там терять нечего.
 * Поле считается, пока оно в документе, не пустое и не лежит в закрытом листе.
 */
type TextField = HTMLInputElement | HTMLTextAreaElement;

const SKIPPED_TYPES = new Set(['checkbox', 'radio', 'range', 'file', 'search', 'button', 'submit', 'reset', 'color', 'hidden']);
const SKIPPED_ZONES = '[data-composer], [role="search"]';

function textField(target: EventTarget | null): TextField | null {
  if (target instanceof HTMLTextAreaElement) return target;
  if (target instanceof HTMLInputElement && !SKIPPED_TYPES.has(target.type)) return target;
  return null;
}

export interface TypedText {
  /** На странице есть набранный и ещё не ушедший текст. */
  pending(): boolean;
  destroy(): void;
}

export function trackTypedText(root: Document): TypedText {
  const touched = new Set<TextField>();
  const input = (event: Event) => {
    const field = textField(event.target);
    if (field && !field.closest(SKIPPED_ZONES)) touched.add(field);
  };
  root.addEventListener('input', input, true);
  return {
    pending() {
      for (const field of touched) {
        if (!field.isConnected) touched.delete(field);
        else if (field.value !== '' && !field.closest('dialog:not([open])')) return true;
      }
      return false;
    },
    destroy() {
      root.removeEventListener('input', input, true);
      touched.clear();
    }
  };
}
