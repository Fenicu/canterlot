/** Подсказка в поле ввода: во время хода она честно обещает очередь, а не отправку. */
export function composerPlaceholder(live: boolean, suggestion: string | null, idle = 'Сообщение агенту…'): string {
  if (live) return 'Ход идёт: сообщение встанет в очередь';
  return suggestion ?? idle;
}

/** Имя кнопки отправки: во время хода она ставит сообщение в очередь. */
export function sendActionLabel(live: boolean): string {
  return live ? 'В очередь' : 'Отправить';
}
