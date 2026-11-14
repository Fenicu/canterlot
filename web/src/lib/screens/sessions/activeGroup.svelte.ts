/** Открытая группа списка: из неё форма «Новая сессия» берёт прошлые настройки. */
let group = $state("");

export function activeGroupId(): string {
  return group;
}

export function setActiveGroupId(id: string): void {
  group = id;
}
