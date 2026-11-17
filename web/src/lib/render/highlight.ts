import { sanitizeRendered } from './sanitize';

export const HIGHLIGHT_LIMITS = {
  maxBytes: 20480,
  maxLines: 400
} as const;

// Пути заданы явно: шаблонную строку в import() сборщик не разрешает, и языки не попадали бы в бандл.
const LOADERS: Record<string, () => Promise<{ default: unknown }>> = {
  rust: () => import('highlight.js/lib/languages/rust'),
  typescript: () => import('highlight.js/lib/languages/typescript'),
  javascript: () => import('highlight.js/lib/languages/javascript'),
  python: () => import('highlight.js/lib/languages/python'),
  bash: () => import('highlight.js/lib/languages/bash'),
  json: () => import('highlight.js/lib/languages/json'),
  yaml: () => import('highlight.js/lib/languages/yaml'),
  toml: () => import('highlight.js/lib/languages/ini'),
  sql: () => import('highlight.js/lib/languages/sql'),
  diff: () => import('highlight.js/lib/languages/diff'),
  xml: () => import('highlight.js/lib/languages/xml'),
  go: () => import('highlight.js/lib/languages/go'),
  css: () => import('highlight.js/lib/languages/css'),
  markdown: () => import('highlight.js/lib/languages/markdown')
};

const LANGUAGE_MAP: Record<string, string> = {
  rust: 'rust',
  rs: 'rust',
  typescript: 'typescript',
  ts: 'typescript',
  tsx: 'typescript',
  javascript: 'javascript',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  python: 'python',
  py: 'python',
  bash: 'bash',
  sh: 'bash',
  zsh: 'bash',
  shell: 'bash',
  json: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  sql: 'sql',
  diff: 'diff',
  patch: 'diff',
  xml: 'xml',
  html: 'xml',
  xhtml: 'xml',
  svg: 'xml',
  go: 'go',
  golang: 'go',
  css: 'css',
  markdown: 'markdown',
  md: 'markdown',
  svelte: 'xml' // svelte→xml по требованию ТЗ
};

interface HljsCore {
  registerLanguage(name: string, fn: unknown): void;
  getLanguage(name: string): unknown;
  highlight(code: string, options: { language: string; ignoreIllegals: boolean }): { value: string };
}

let hljsPromise: Promise<HljsCore> | null = null;
const registeredLanguages = new Set<string>();

async function getHljs(): Promise<HljsCore> {
  if (!hljsPromise) {
    hljsPromise = import('highlight.js/lib/core').then(m => m.default as unknown as HljsCore);
  }
  return hljsPromise;
}

export async function highlightLazy(code: string, lang: string | null): Promise<string | null> {
  if (!lang || !lang.trim()) return null;
  const normalized = lang.trim().toLowerCase();
  const canonical = LANGUAGE_MAP[normalized];
  if (!canonical) return null;

  // Проверка лимитов на размер блока
  const byteLength = new TextEncoder().encode(code).length;
  if (byteLength > HIGHLIGHT_LIMITS.maxBytes) return null;
  const lineCount = code.split('\n').length;
  if (lineCount > HIGHLIGHT_LIMITS.maxLines) return null;

  const hljs = await getHljs();

  if (!registeredLanguages.has(canonical)) {
    try {
      const langModule = await LOADERS[canonical]!();
      hljs.registerLanguage(canonical, langModule.default);
      registeredLanguages.add(canonical);
    } catch {
      return null;
    }
  }

  const raw = hljs.highlight(code, { language: canonical, ignoreIllegals: true });
  const clean = sanitizeRendered(raw.value, 'highlight');
  remember(`${canonical}\n${code}`, clean);
  return clean;
}

const CACHE_LIMIT = 128;
const cache = new Map<string, string>();

function remember(key: string, html: string): void {
  if (cache.size >= CACHE_LIMIT) cache.delete(cache.keys().next().value as string);
  cache.set(key, html);
}

function languageOf(code: Element): string | null {
  for (const token of code.classList) if (token.startsWith('language-')) return token.slice('language-'.length);
  return null;
}

function paint(code: Element, html: string): void {
  const parsed = new DOMParser().parseFromString(html, 'text/html');
  code.replaceChildren(...parsed.body.childNodes);
  code.classList.add('hljs');
}

/**
 * Подсвечивает готовые блоки кода внутри узла. Вызывается только для завершённого текста: поток сюда не попадает.
 * Лента виртуальная, поэтому уже подсвеченный блок при возврате в окно красится сразу, без кадра серого текста.
 */
export function highlightWithin(root: HTMLElement): () => void {
  const pending: { code: Element; lang: string; text: string }[] = [];
  for (const code of root.querySelectorAll('pre > code[class*="language-"]')) {
    if (code.classList.contains('hljs')) continue;
    const lang = languageOf(code);
    const canonical = lang ? LANGUAGE_MAP[lang.trim().toLowerCase()] : undefined;
    if (!lang || !canonical) continue;
    const text = code.textContent ?? '';
    const known = cache.get(`${canonical}\n${text}`);
    if (known !== undefined) paint(code, known);
    else pending.push({ code, lang, text });
  }
  if (pending.length === 0) return () => undefined;
  let alive = true;
  const timer = setTimeout(async () => {
    for (const item of pending) {
      const html = await highlightLazy(item.text, item.lang);
      if (!alive) return;
      if (html !== null) paint(item.code, html);
    }
  }, 0);
  return () => {
    alive = false;
    clearTimeout(timer);
  };
}

/** Кусок подсвеченной строки: текст и классы highlight.js всех его обёрток. */
export interface CodeToken {
  text: string;
  cls: string;
}

/** Язык подсветки по имени файла: `a/b.ts` → `ts`; незнакомое расширение — `null`. */
export function languageForPath(path: string): string | null {
  const name = path.split('/').pop() ?? '';
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return null;
  const ext = name.slice(dot + 1).toLowerCase();
  return LANGUAGE_MAP[ext] ? ext : null;
}

/**
 * Разметка highlight.js построчно. Обёртка может пересекать перевод строки (многострочная
 * строка, комментарий): тогда её класс достаётся каждой строке, куда она дотянулась.
 */
export function htmlLines(html: string): CodeToken[][] {
  const lines: CodeToken[][] = [[]];
  const walk = (node: Node, cls: string[]) => {
    if (node.nodeType === Node.TEXT_NODE) {
      const parts = (node.textContent ?? '').split('\n');
      parts.forEach((part, index) => {
        if (index > 0) lines.push([]);
        if (part) lines[lines.length - 1]!.push({ text: part, cls: cls.join(' ') });
      });
      return;
    }
    const own = node instanceof Element ? [...node.classList] : [];
    for (const child of node.childNodes) walk(child, [...cls, ...own]);
  };
  walk(new DOMParser().parseFromString(`<body>${html}</body>`, 'text/html').body, []);
  return lines;
}

/** Код построчно с подсветкой; `null` — язык незнаком или код больше пределов подсветки. */
export async function highlightLines(code: string, lang: string | null): Promise<CodeToken[][] | null> {
  const html = await highlightLazy(code, lang);
  return html === null ? null : htmlLines(html);
}
