/**
 * Действие Svelte: сообщает, виден ли элемент в окне. Один общий наблюдатель на всех, поэтому
 * сотня строк списка стоит одного `IntersectionObserver`. Нужен там, где движение идёт, пока
 * дело идёт, но смотреть на него можно только на видимых строках (дыхание кольца сессии):
 * ушедшие за край и за прокрутку строки стоят ровно.
 *
 * Без `IntersectionObserver` (окружения без вёрстки) узел считается видимым: движение остаётся
 * делом CSS и настройки движения, а не этого действия.
 */
const watchers = new WeakMap<Element, (visible: boolean) => void>();
let shared: IntersectionObserver | null = null;

function observer(): IntersectionObserver | null {
  if (typeof IntersectionObserver === 'undefined') return null;
  shared ??= new IntersectionObserver((entries) => {
    for (const entry of entries) watchers.get(entry.target)?.(entry.isIntersecting);
  });
  return shared;
}

export function inView(node: Element, report: (visible: boolean) => void) {
  const watching = observer();
  watchers.set(node, report);
  if (watching) watching.observe(node);
  else report(true);
  return {
    update(next: (visible: boolean) => void) {
      watchers.set(node, next);
    },
    destroy() {
      watching?.unobserve(node);
      watchers.delete(node);
    }
  };
}
