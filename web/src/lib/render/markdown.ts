import MarkdownIt from 'markdown-it';
import DOMPurify from 'dompurify';
import type { DOMPurify as DOMPurifyInstance } from 'dompurify';

export interface MdRenderer {
  render(text: string): string;
}

export const ALLOWED_TAGS = [
  'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li',
  'pre', 'code', 'em', 'strong', 'del', 'blockquote', 'hr',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'a', 'br', 'span'
];

export const ALLOWED_ATTR = ['href', 'title', 'class', 'rel', 'target', 'start'];

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

export function createMarkdown(): MdRenderer {
  const md = new MarkdownIt({
    html: false,
    linkify: true,
    typographer: false
  });

  // Строгая валидация схем ссылок в markdown-it
  md.validateLink = (url: string) => isSafeUrl(url);

  // Замена изображений на ссылки с их alt-текстом (никаких <img>)
  md.renderer.rules.image = (tokens, idx, options, env, self) => {
    const token = tokens[idx];
    if (!token) return '';
    const alt = self.renderInline(token.children || [], options, env);
    const src = String(token.attrGet('src') || '');
    const title = token.attrGet('title');

    if (isSafeUrl(src)) {
      const titleAttr = title ? ` title="${md.utils.escapeHtml(String(title))}"` : '';
      const safeSrc = md.utils.escapeHtml(String(src));
      return `<a href="${safeSrc}"${titleAttr} target="_blank" rel="noopener noreferrer nofollow">${alt || safeSrc}</a>`;
    }
    return alt;
  };

  // Все ссылки открываются в новой вкладке с noopener noreferrer nofollow
  const defaultLinkOpen = md.renderer.rules.link_open || ((tokens, idx, opt, _env, self) => self.renderToken(tokens, idx, opt));
  md.renderer.rules.link_open = (tokens, idx, opt, env, self) => {
    const token = tokens[idx];
    if (token) {
      token.attrSet('target', '_blank');
      token.attrSet('rel', 'noopener noreferrer nofollow');
    }
    return defaultLinkOpen(tokens, idx, opt, env, self);
  };

  const purify = getPurify();

  // Хук очистки атрибутов и классов
  purify.removeHook('afterSanitizeAttributes');
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
        .filter(cls => cls.startsWith('hljs') || cls.startsWith('language-'));
      if (allowed.length > 0) {
        node.setAttribute('class', allowed.join(' '));
      } else {
        node.removeAttribute('class');
      }
    }
  });

  return {
    render(text: string): string {
      if (!text) return '';
      const raw = md.render(text);
      return purify.sanitize(raw, {
        ALLOWED_TAGS,
        ALLOWED_ATTR,
        ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto):|#)/i
      });
    }
  };
}
