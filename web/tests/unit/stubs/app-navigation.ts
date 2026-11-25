import { page } from './app-state.svelte.js';

/**
 * Замена `$app/navigation` в юнит-тестах. Поверхностный переход кладёт состояние страницы в запись истории
 * jsdom не сразу, как и настоящий; «Назад» возвращает прежнее состояние через `followHistory`.
 */
export async function goto(_url: string, options: { state?: App.PageState } = {}): Promise<void> {
  await Promise.resolve();
  const state = options.state ?? {};
  history.pushState(state, '');
  page.state = state;
}

/** Подписка на «Назад»; возвращает отписку. */
export function followHistory(): () => void {
  const pop = (event: PopStateEvent) => {
    page.state = (event.state as App.PageState | null) ?? {};
  };
  addEventListener('popstate', pop);
  return () => removeEventListener('popstate', pop);
}
