// Поиск в сети и чтение страницы.
import { fmt } from '#lib/format/ru.js';
import { str } from '../json';
import { clip, say } from '../copy';
import type { CardLogic } from '../types';

export interface SearchData {
  query: string;
  /** Ссылок в ответе; `null`, пока ответа нет или он без ссылок. */
  count: number | null;
}

/** Ссылки в ответе поиска: у Claude Code это `Links: [{"title","url"}]`, у остальных — адреса в тексте. */
export function linkCount(text: string): number | null {
  if (!text) return null;
  const listed = text.match(/"url"\s*:\s*"https?:\/\//g)?.length ?? 0;
  if (listed > 0) return listed;
  const urls = new Set(text.match(/https?:\/\/[^\s)"'\]>]+/g) ?? []);
  return urls.size > 0 ? urls.size : null;
}

export const search: CardLogic<SearchData> = {
  label: 'Поиск',
  parse(call) {
    const query = str(call.input.query) || str(call.input.q) || call.argument;
    if (!query) return null;
    return { query, count: linkCount(call.output) };
  },
  summary(call, data) {
    const head = call.state === 'running' ? say(call.voice, 'Ищет', 'Поиск') : say(call.voice, 'Искала', 'Поиск');
    const count = data.count !== null ? ` · ${data.count} ${fmt.plural(data.count, ['ссылка', 'ссылки', 'ссылок'])}` : '';
    return `${head}: ${clip(data.query, 90)}${count}`;
  }
};

export interface FetchData {
  url: string;
  domain: string;
  /** Заголовок страницы или первый заголовок ответа. */
  title: string;
}

export function domainOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, '');
  } catch {
    return '';
  }
}

/** Заголовок из ответа: `<title>`, первый заголовок разметки, иначе первая строка. */
export function pageTitle(text: string): string {
  const html = /<title[^>]*>([^<]{1,200})<\/title>/i.exec(text);
  if (html) return html[1]!.trim();
  const heading = /^\s{0,3}#{1,3}\s+(.+)$/m.exec(text);
  if (heading) return heading[1]!.replace(/[*_`]/g, '').trim();
  const first = text.split('\n').find((line) => line.trim())?.trim() ?? '';
  return first.length <= 120 ? first : '';
}

export const fetchPage: CardLogic<FetchData> = {
  label: 'Страница',
  parse(call) {
    const url = str(call.input.url) || (/^https?:\/\//.test(call.argument) ? call.argument.split(/\s/)[0]! : '');
    const domain = domainOf(url);
    if (!domain) return null;
    return { url, domain, title: call.state === 'error' ? '' : clip(pageTitle(call.output), 100) };
  },
  summary(call, data) {
    const head = say(call.voice, `Открыла ${data.domain}`, `Страница ${data.domain}`);
    return data.title ? `${head} · ${data.title}` : head;
  }
};
