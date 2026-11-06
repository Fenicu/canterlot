import createDOMPurify from 'dompurify';
import type { DOMPurify as DOMPurifyInstance } from 'dompurify';

export const ALLOWED_TAGS = [
  'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li',
  'pre', 'code', 'em', 'strong', 'del', 'blockquote', 'hr',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'a', 'br', 'span'
];

export const ALLOWED_ATTR = ['href', 'title', 'class', 'rel', 'target', 'start', 'role', 'aria-checked', 'aria-readonly', 'aria-label'];

function decodeScheme(url: string): string {
  let s = url.trim();
  s = s.replace(/[\u0000-\u001f\u007f-\u009f]/g, '');
  s = s.replace(/&#x([0-9a-f]+);?/gi, (_, hex) => String.fromCharCode(parseInt(hex, 16)));
  s = s.replace(/&#([0-9]+);?/g, (_, dec) => String.fromCharCode(parseInt(dec, 10)));
  s = s.replace(/&(?:Tab|NewLine);/gi, '');
  try {
    s = decodeURIComponent(s);
  } catch {
    // Игнорируем некорректное процентное кодирование
  }
  return s.replace(/[\s\u0000-\u001f\u007f-\u009f]/g, '').toLowerCase();
}

export function isSafeUrl(url: string): boolean {
  if (!url) return false;
  const decoded = decodeScheme(url);
  return /^(https?:|mailto:|#)/.test(decoded);
}

let purify: DOMPurifyInstance | undefined;

function getPurify(): DOMPurifyInstance {
  if (typeof window === 'undefined') throw new Error('DOMPurify requires a DOM environment');
  if (purify) return purify;
  // Calling the factory isolates our hooks from the imported singleton.
  purify = createDOMPurify(window);
  purify.addHook('afterSanitizeAttributes', (node: Element) => {
    const tag = node.tagName.toLowerCase();
    if (tag === 'a') {
      const href = node.getAttribute('href') || '';
      if (!isSafeUrl(href)) {
        node.removeAttribute('href');
      } else {
        node.setAttribute('target', '_blank');
        node.setAttribute('rel', 'noopener noreferrer nofollow');
      }
    }

    if (node.hasAttribute('class')) {
      const rawClass = node.getAttribute('class') || '';
      const allowed = rawClass
        .split(/\s+/)
        .filter(cls => cls.startsWith('hljs') || cls.startsWith('language-') || cls.startsWith('task-'));
      if (allowed.length > 0) {
        node.setAttribute('class', allowed.join(' '));
      } else {
        node.removeAttribute('class');
      }
    }
  });

  return purify;
}

export function sanitizeRendered(html: string, kind: 'markdown' | 'highlight'): string {
  return getPurify().sanitize(html, {
    ALLOWED_TAGS: kind === 'highlight' ? ['span'] : ALLOWED_TAGS,
    ALLOWED_ATTR: kind === 'highlight' ? ['class'] : ALLOWED_ATTR,
    ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto):|#)/i,
    // DOMPurify проверяет этим выражением значения всех атрибутов, кроме безопасных, а номер списка не ссылка.
    ADD_URI_SAFE_ATTR: ['start'],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: true
  });
}
