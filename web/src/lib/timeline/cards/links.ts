import { quoted } from './copy';
import { sessionTitle } from './defs/sessions';
import type { CardCall, CardLink } from './types';

/** Ссылка на сессию из строки карточки: заголовок в кавычках ведёт в её ленту. */
export function sessionLink(call: CardCall, id: string, fallback = ''): CardLink | null {
  if (!id) return null;
  return { text: quoted(sessionTitle(call, id, fallback)), href: `/c/${id}` };
}

/** Строка, разбитая вокруг ссылки: до, ссылка, после. Ссылки нет в строке — она целиком текстом. */
export function splitLink(line: string, link: CardLink | null): { before: string; link: CardLink | null; after: string } {
  if (!link) return { before: line, link: null, after: '' };
  const at = line.indexOf(link.text);
  if (at < 0) return { before: line, link: null, after: '' };
  return { before: line.slice(0, at), link, after: line.slice(at + link.text.length) };
}
