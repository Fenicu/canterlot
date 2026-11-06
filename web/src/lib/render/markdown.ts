import MarkdownIt from 'markdown-it';
import { isSafeUrl, sanitizeRendered } from './sanitize';

export { ALLOWED_TAGS, ALLOWED_ATTR, isSafeUrl } from './sanitize';

export interface MdRenderer {
  render(text: string): string;
}

/** Путь внутри документа некуда вести: у просмотрщика нет его каталога. */
function isRelativeUrl(url: string): boolean {
  const trimmed = url.trim();
  if (!trimmed || trimmed.startsWith('//')) return false;
  return !/^[a-z][a-z0-9+.-]*:/i.test(trimmed);
}

export function createMarkdown(): MdRenderer {
  const md = new MarkdownIt({
    html: false,
    linkify: true,
    typographer: false
  });

  // Строгая валидация схем ссылок в markdown-it. Относительные адреса разбираем,
  // чтобы ниже оставить от ссылки текст, а от картинки — подпись.
  md.validateLink = (url: string) => isSafeUrl(url) || isRelativeUrl(url);

  // Безопасные адреса идут обычным путём; остальные (относительные) теряют разметку ссылки.
  md.core.ruler.after('inline', 'neutralize_relative_links', (state) => {
    for (const token of state.tokens) {
      if (token.type !== 'inline' || !token.children) continue;
      const stack: boolean[] = [];
      for (const child of token.children) {
        if (child.type === 'link_open') {
          const plain = !isSafeUrl(String(child.attrGet('href') ?? ''));
          child.meta = { ...(child.meta ?? {}), plain };
          stack.push(plain);
        } else if (child.type === 'link_close') {
          child.meta = { ...(child.meta ?? {}), plain: stack.pop() ?? false };
        }
      }
    }
    return true;
  });

  // Отметки задач GFM: маркер рисует CSS, состояние остаётся в aria у самого маркера.
  md.core.ruler.after('inline', 'task_list_marks', (state) => {
    let open: (typeof state.tokens)[number] | null = null;
    let seen = false;
    for (const token of state.tokens) {
      if (token.type === 'list_item_open') {
        open = token;
        seen = false;
        continue;
      }
      if (token.type === 'list_item_close') {
        open = null;
        continue;
      }
      if (!open || seen || token.type !== 'inline' || !token.children) continue;
      seen = true;
      const first = token.children[0];
      if (!first || first.type !== 'text') continue;
      const match = /^\[([ xX])\]\s+/.exec(first.content);
      if (!match) continue;

      const done = match[1] !== ' ';
      first.content = first.content.slice(match[0].length);
      open.attrJoin('class', done ? 'task-item task-item--done' : 'task-item');
      const marker = new state.Token('task_mark', '', 0);
      marker.meta = { done };
      token.children.unshift(marker);
    }
    return true;
  });

  md.renderer.rules.task_mark = (tokens, idx) => {
    const done = tokens[idx]?.meta?.done === true;
    return `<span class="task-marker${done ? ' task-marker--done' : ''}" role="checkbox" aria-checked="${done}" aria-readonly="true" aria-label="${done ? 'выполнено' : 'не выполнено'}"></span>`;
  };

  // Замена изображений на ссылки с их alt-текстом (никаких <img>). Внутренний путь
  // открыть нельзя, поэтому от такой картинки остаётся только подпись.
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
    if (token?.meta?.plain) return '';
    if (token) {
      token.attrSet('target', '_blank');
      token.attrSet('rel', 'noopener noreferrer nofollow');
    }
    return defaultLinkOpen(tokens, idx, opt, env, self);
  };

  const defaultLinkClose = md.renderer.rules.link_close || ((tokens, idx, opt, _env, self) => self.renderToken(tokens, idx, opt));
  md.renderer.rules.link_close = (tokens, idx, opt, env, self) => {
    if (tokens[idx]?.meta?.plain) return '';
    return defaultLinkClose(tokens, idx, opt, env, self);
  };

  return {
    render(text: string): string {
      if (!text) return '';
      const raw = md.render(text);
      return sanitizeRendered(raw, 'markdown');
    }
  };
}

/** Один разборщик на приложение: создание MarkdownIt дороже самого рендера. */
export const sharedMarkdown = createMarkdown();
