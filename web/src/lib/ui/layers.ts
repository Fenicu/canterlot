/**
 * Открытые модальные слои (листы, просмотр медиа) в порядке открытия.
 *
 * Оболочка `inert`, пока открыт хотя бы один слой. Слой не запоминает прежнее значение `inert` сам: слои
 * закрываются не в том порядке, в каком открылись, и нижний, закрывшись раньше верхнего, оставлял оболочку
 * заблокированной при пустом экране.
 */
const open: object[] = [];

function sync(): void {
  const shell = document.getElementById('app-shell');
  if (shell) shell.inert = open.length > 0;
}

/** Слой открылся; метку вернуть в `dropLayer`. */
export function pushLayer(): object {
  const layer = {};
  open.push(layer);
  sync();
  return layer;
}

/** Слой закрылся. Повторный вызов и `null` безвредны. */
export function dropLayer(layer: object | null): void {
  const index = layer ? open.indexOf(layer) : -1;
  if (index < 0) return;
  open.splice(index, 1);
  sync();
}

/** Поверх слоя ничего не открыто. */
export function isTopLayer(layer: object | null): boolean {
  return layer !== null && open.at(-1) === layer;
}
