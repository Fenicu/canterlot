import DOMPurify from 'dompurify';
import type { DOMPurify as DOMPurifyInstance } from 'dompurify';

export const HIGHLIGHT_LIMITS = {
  maxBytes: 20480,
  maxLines: 400
} as const;

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

function getPurify(): DOMPurifyInstance {
  if (typeof window !== 'undefined') {
    const raw = DOMPurify as unknown as { default?: (w: unknown) => DOMPurifyInstance; sanitize?: unknown };
    if (typeof (raw as { sanitize?: unknown }).sanitize === 'function') {
      return raw as unknown as DOMPurifyInstance;
    }
    if (typeof raw.default === 'function') {
      return raw.default(window);
    }
    return (DOMPurify as unknown as (w: unknown) => DOMPurifyInstance)(window);
  }
  throw new Error('DOMPurify requires a DOM environment');
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
      const langModule = await import(`highlight.js/lib/languages/${canonical}`);
      hljs.registerLanguage(canonical, langModule.default);
      registeredLanguages.add(canonical);
    } catch {
      return null;
    }
  }

  const raw = hljs.highlight(code, { language: canonical, ignoreIllegals: true });
  const purify = getPurify();

  // Очистка HTML подсветки через строгий allow-list DOMPurify
  return purify.sanitize(raw.value, {
    ALLOWED_TAGS: ['span'],
    ALLOWED_ATTR: ['class']
  });
}
