import MarkdownIt from 'markdown-it';
import { isSafeUrl, sanitizeRendered } from './sanitize';

export { ALLOWED_TAGS, ALLOWED_ATTR, isSafeUrl } from './sanitize';

export interface MdRenderer {
  render(text: string): string;
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

  return {
    render(text: string): string {
      if (!text) return '';
      const raw = md.render(text);
      return sanitizeRendered(raw, 'markdown');
    }
  };
}
