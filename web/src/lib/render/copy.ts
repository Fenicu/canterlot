import MarkdownIt from 'markdown-it';
import { icons } from '#lib/ui/icons.js';

const outline = new MarkdownIt({ html: false });

export const COPY_CODE_LABEL = 'Скопировать код';
export const COPY_QUOTE_LABEL = 'Скопировать цитату';
export const COPY_DONE = 'Скопировано';
export const COPY_FAILED = 'Не удалось скопировать';

const quoteText = new WeakMap<HTMLButtonElement, string>();
const statusTimers = new WeakMap<HTMLButtonElement, ReturnType<typeof setTimeout>>();

function unquote(line: string): string {
  return line.replace(/^ {0,3}> ?/, '');
}

/**
 * Внутренний markdown верхнеуровневых цитат в порядке появления.
 * Диапазоны строк берутся у markdown-it: это ровно те строки, из которых собрана цитата.
 */
export function blockquoteSources(source: string): string[] {
  if (!source) return [];
  const lines = source.split(/\r?\n/);
  const sources: string[] = [];
  for (const token of outline.parse(source, {})) {
    if (token.type !== 'blockquote_open' || token.level !== 0 || !token.map) continue;
    const [start, end] = token.map;
    sources.push(lines.slice(start, end).map(unquote).join('\n'));
  }
  return sources;
}

/** Текст блока кода как есть: без нумерации строк и без добавленного перевода строки. */
export function codeCopyText(pre: Element): string {
  const code = pre.querySelector('code');
  return code ? code.textContent ?? '' : pre.textContent ?? '';
}

const SVG_NS = 'http://www.w3.org/2000/svg';

function makeButton(kind: 'code' | 'quote', label: string): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'copy-button';
  button.dataset.copy = kind;
  button.setAttribute('aria-label', label);
  const icon = document.createElementNS(SVG_NS, 'svg');
  icon.setAttribute('viewBox', '0 0 24 24');
  icon.setAttribute('aria-hidden', 'true');
  icon.setAttribute('focusable', 'false');
  const path = document.createElementNS(SVG_NS, 'path');
  path.setAttribute('d', icons.copy);
  icon.append(path);
  const status = document.createElement('span');
  status.className = 'copy-status';
  status.setAttribute('role', 'status');
  button.append(icon, status);
  return button;
}

/** Код получает кнопку в обёртке: она не уезжает при горизонтальной прокрутке блока. */
export function decorateCode(root: HTMLElement): void {
  for (const pre of root.querySelectorAll<HTMLPreElement>('pre')) {
    if (pre.parentElement?.classList.contains('code-block')) continue;
    const frame = document.createElement('div');
    frame.className = 'code-block';
    pre.before(frame);
    frame.append(pre, makeButton('code', COPY_CODE_LABEL));
  }
}

/** Кнопка цитаты знает исходник блока: в буфер уходит markdown, а не текст с экрана. */
export function decorateQuotes(root: HTMLElement, sources: string[]): void {
  root.querySelectorAll(':scope > blockquote').forEach((quote, index) => {
    const source = sources[index];
    const existing = quote.querySelector<HTMLButtonElement>(':scope > .copy-button');
    if (source === undefined) {
      existing?.remove();
      return;
    }
    if (existing) {
      quoteText.set(existing, source);
      return;
    }
    const button = makeButton('quote', COPY_QUOTE_LABEL);
    quoteText.set(button, source);
    quote.append(button);
  });
}

function showStatus(button: HTMLButtonElement, text: string): void {
  const status = button.querySelector('.copy-status');
  if (!status) return;
  status.textContent = text;
  button.dataset.copied = text === COPY_DONE ? 'done' : 'failed';
  const timer = statusTimers.get(button);
  if (timer) clearTimeout(timer);
  statusTimers.set(
    button,
    setTimeout(() => {
      status.textContent = '';
      delete button.dataset.copied;
      statusTimers.delete(button);
    }, 2000)
  );
}

export function copyButtonFrom(target: EventTarget | null): HTMLButtonElement | null {
  if (!(target instanceof Element)) return null;
  const button = target.closest('button.copy-button');
  return button instanceof HTMLButtonElement ? button : null;
}

export async function copyBlock(button: HTMLButtonElement): Promise<void> {
  let text = '';
  if (button.dataset.copy === 'quote') {
    text = quoteText.get(button) ?? '';
  } else {
    const pre = button.closest('.code-block')?.querySelector('pre');
    if (pre) text = codeCopyText(pre);
  }
  try {
    if (typeof navigator === 'undefined' || !navigator.clipboard?.writeText) {
      throw new Error('clipboard_unavailable');
    }
    await navigator.clipboard.writeText(text);
    showStatus(button, COPY_DONE);
  } catch {
    showStatus(button, COPY_FAILED);
  }
}
