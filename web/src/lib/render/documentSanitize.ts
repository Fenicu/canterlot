import createDOMPurify from 'dompurify';
import type { DOMPurify as DOMPurifyInstance } from 'dompurify';
import { decodeScheme, isSafeUrl } from './sanitize.js';

// Отдельный модуль, а не часть sanitize.ts: тот грузится с лентой, этот — только с просмотрщиком документов.
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
