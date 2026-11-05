// Карточка входит один раз за жизнь приложения: возврат в чат и пересборка экрана её не повторяют.
const arrived = new Set<string>();

/** Первое появление запроса у поля ввода: только оно показывается движением. */
export function firstArrival(requestId: string): boolean {
  if (arrived.has(requestId)) return false;
  arrived.add(requestId);
  return true;
}
