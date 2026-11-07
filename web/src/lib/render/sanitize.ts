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

/** Разметка из .docx: то, что выдаёт mammoth, без стилей, атрибутов id и встроенных обработчиков. */
export const DOCUMENT_TAGS = [
  'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li', 'blockquote', 'pre', 'code',
  'strong', 'em', 'u', 's', 'del', 'sup', 'sub', 'br', 'a', 'img',
  'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td'
];
export const DOCUMENT_ATTR = ['href', 'title', 'src', 'alt', 'colspan', 'rowspan'];

/** Метка картинки, которую ставит разборщик .docx: `docx-image:<номер>`. */
const DOCUMENT_IMAGE = /^docx-image:(\d+)$/;

let documentPurify: DOMPurifyInstance | undefined;
let documentImages: readonly string[] = [];

function getDocumentPurify(): DOMPurifyInstance {
  if (typeof window === 'undefined') throw new Error('DOMPurify requires a DOM environment');
  if (documentPurify) return documentPurify;
  documentPurify = createDOMPurify(window);
  documentPurify.addHook('afterSanitizeAttributes', (node: Element) => {
    const tag = node.tagName.toLowerCase();
    if (tag === 'a') {
      // Внутренние якоря документа ведут в никуда: атрибутов id в разметке нет, а смена адреса закрыла бы лист.
      const href = node.getAttribute('href') || '';
      if (isSafeUrl(href) && !decodeScheme(href).startsWith('#')) {
        node.setAttribute('target', '_blank');
        node.setAttribute('rel', 'noopener noreferrer nofollow');
      } else {
        node.removeAttribute('href');
      }
    }
    if (tag === 'img') {
      // Картинка показывается только из самого файла и только через выданный просмотрщиком адрес blob:.
      const index = DOCUMENT_IMAGE.exec(node.getAttribute('src') || '')?.[1];
      const url = index === undefined ? undefined : documentImages[Number(index)];
      if (url?.startsWith('blob:')) node.setAttribute('src', url);
      else node.parentNode?.removeChild(node);
    }
  });
  return documentPurify;
}

/**
 * Очищает HTML, собранный из .docx. Метки `docx-image:N` заменяются адресами из `images`,
 * любые другие картинки (сеть, data:, файловые пути) выбрасываются целиком.
 */
export function sanitizeDocument(html: string, images: readonly string[] = []): string {
  const purify = getDocumentPurify();
  documentImages = images;
  try {
    return purify.sanitize(html, {
      ALLOWED_TAGS: DOCUMENT_TAGS,
      ALLOWED_ATTR: DOCUMENT_ATTR,
      ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto):|#|docx-image:\d+$)/i,
      // Как и номер списка в Markdown: размах ячейки — число, а не ссылка, выражение выше к нему не относится.
      ADD_URI_SAFE_ATTR: ['colspan', 'rowspan'],
      ALLOW_DATA_ATTR: false,
      ALLOW_ARIA_ATTR: false
    });
  } finally {
    documentImages = [];
  }
}
