import { quoted } from './copy';
import { sessionTitle } from './defs/sessions';
import type { CardCall, CardLink } from './types';

/** Ссылка на сессию из строки карточки: заголовок в кавычках ведёт в её ленту. */
export function sessionLink(call: CardCall, id: string, fallback = ''): CardLink | null {
  if (!id) return null;
  return { text: quoted(sessionTitle(call, id, fallback)), href: `/c/${id}` };
}

/**
 * Внешняя ссылка карточки открывается в новой вкладке, а её адрес приходит из аргументов
 * вызова или ответа инструмента. Наружу ведёт только http(s): `intent://` и схемы приложений
 * на телефоне по касанию открыли бы чужую программу. Остальное остаётся текстом строки.
 */
export function safeLink(link: CardLink | null): CardLink | null {
  if (!link?.external) return link;
  try {
    const { protocol } = new URL(link.href);
    return protocol === 'http:' || protocol === 'https:' ? link : null;
  } catch {
    return null;
  }
}

/** Строка, разбитая вокруг ссылки: до, ссылка, после. Ссылки нет в строке — она целиком текстом. */
export function splitLink(line: string, link: CardLink | null): { before: string; link: CardLink | null; after: string } {
  if (!link) return { before: line, link: null, after: '' };
  const at = line.indexOf(link.text);
  if (at < 0) return { before: line, link: null, after: '' };
  return { before: line.slice(0, at), link, after: line.slice(at + link.text.length) };
}
