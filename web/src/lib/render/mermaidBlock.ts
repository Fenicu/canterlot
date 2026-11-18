import {
  mermaidPhase,
  mermaidThemeVariables,
  renderMermaidSvg,
  type MermaidPhase
} from './mermaid.js';

export interface MermaidBlockInput {
  streaming: boolean;
  themeKey: string;
}

interface Instance {
  id: string;
  code: string;
  pre: HTMLPreElement;
  wrap: HTMLElement;
  view: HTMLElement;
  error: HTMLElement;
  codeButton: HTMLButtonElement;
  diagramButton: HTMLButtonElement;
  phase: MermaidPhase;
  mode: 'code' | 'diagram';
  failed: boolean;
  ready: boolean;
  started: boolean;
  disposeCss: (() => void) | null;
}

function sourceOf(instance: Instance): HTMLElement {
  return instance.pre.closest<HTMLElement>('.code-block') ?? instance.pre;
}

/** constructable stylesheet не попадает под style-src: CSP разрешает только встроенные <style> и атрибуты. */
function adoptCss(id: string, css: string): () => void {
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(css);
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  return () => {
    document.adoptedStyleSheets = document.adoptedStyleSheets.filter((item) => item !== sheet);
  };
}

function fallbackCss(id: string, css: string): () => void {
  const style = document.createElement('style');
  style.dataset.mermaid = id;
  style.textContent = css;
  document.head.append(style);
  return () => style.remove();
}

function addCss(id: string, css: string): () => void {
  try {
    return adoptCss(id, css);
  } catch {
    return fallbackCss(id, css);
  }
}

/**
 * Заменяет завершённые блоки ```mermaid на переключатель «Код / Диаграмма».
 * Пока блок идёт, разметка не трогается: исходник остаётся обычным кодом.
 */
export function mermaidWithin(node: HTMLElement, value: MermaidBlockInput) {
  let current = value;
  let disposed = false;
  const instances: Instance[] = [];
  let observer: IntersectionObserver | null = null;
  let idSequence = 0;

  function tokens(): Record<string, string> {
    const style = getComputedStyle(document.documentElement);
    return mermaidThemeVariables((name) => style.getPropertyValue(name));
  }

  function apply(instance: Instance, next: MermaidPhase): void {
    instance.phase = next;
    instance.wrap.dataset.phase = next;
    const showCode = next !== 'diagram';
    sourceOf(instance).classList.toggle('mermaid-source-hidden', !showCode);
    instance.view.hidden = !instance.ready || showCode;
    instance.error.hidden = next !== 'error';
    instance.codeButton.setAttribute('aria-pressed', String(showCode));
    instance.diagramButton.setAttribute('aria-pressed', String(!showCode));
  }

  async function render(instance: Instance): Promise<void> {
    if (disposed || instance.mode === 'code' || instance.ready) return;
    apply(instance, mermaidPhase({ streaming: false, failed: false, ready: false, mode: instance.mode }));
    try {
      const result = await renderMermaidSvg(instance.code, current.themeKey, tokens());
      if (disposed) return;
      // Разметка уже прошла DOMPurify в SVG-профиле; вставляем узлы, а не html-строку.
      const parsed = new DOMParser().parseFromString(result.markup, 'image/svg+xml');
      instance.view.replaceChildren(document.importNode(parsed.documentElement, true));
      instance.disposeCss?.();
      // Естественный размер из viewBox/max-width: маленькие схемы не растягиваются,
      // широкие остаются читаемыми и прокручиваются внутри .mermaid-view.
      const natural = result.natural;
      const sizing = natural
        ? `#${instance.id} svg{width:${natural.width}px;min-width:min(${natural.width}px,640px);max-width:100%;height:auto}`
        : '';
      instance.disposeCss = addCss(instance.id, `${result.css}\n${sizing}`);
      instance.failed = false;
      instance.ready = true;
      apply(instance, mermaidPhase({ streaming: false, failed: false, ready: true, mode: instance.mode }));
    } catch {
      if (disposed) return;
      instance.failed = true;
      instance.ready = false;
      apply(instance, mermaidPhase({ streaming: false, failed: true, ready: false, mode: instance.mode }));
    }
  }

  function activate(instance: Instance): void {
    if (instance.started) return;
    instance.started = true;
    void render(instance);
  }

  function setup(): void {
    for (const code of Array.from(node.querySelectorAll<HTMLElement>('code.language-mermaid'))) {
      const pre = code.closest('pre');
      if (!pre || pre.dataset.mermaidReady === '1') continue;
      pre.dataset.mermaidReady = '1';
      const id = `mermaid-${idSequence++}`;
      const wrap = document.createElement('div');
      wrap.className = 'mermaid-block';
      wrap.dataset.phase = 'loading';
      wrap.id = `${id}-wrap`;

      const toolbar = document.createElement('div');
      toolbar.className = 'mermaid-toolbar';
      toolbar.setAttribute('role', 'group');
      toolbar.setAttribute('aria-label', 'Вид диаграммы');
      const codeButton = document.createElement('button');
      codeButton.type = 'button';
      codeButton.className = 'mermaid-tab';
      codeButton.textContent = 'Код';
      const diagramButton = document.createElement('button');
      diagramButton.type = 'button';
      diagramButton.className = 'mermaid-tab';
      diagramButton.textContent = 'Диаграмма';
      toolbar.append(codeButton, diagramButton);

      const view = document.createElement('div');
      view.className = 'mermaid-view';
      view.setAttribute('role', 'img');
      view.setAttribute('aria-label', 'Диаграмма');
      const error = document.createElement('div');
      error.className = 'mermaid-error';
      error.setAttribute('role', 'status');
      error.textContent = 'Не удалось построить диаграмму';
      error.hidden = true;

      wrap.append(toolbar, view, error);
      const reference = pre.closest<HTMLElement>('.code-block') ?? pre;
      reference.after(wrap);

      const instance: Instance = {
        id,
        code: code.textContent ?? '',
        pre,
        wrap,
        view,
        error,
        codeButton,
        diagramButton,
        phase: 'loading',
        mode: 'diagram',
        failed: false,
        ready: false,
        started: false,
        disposeCss: null
      };
      instances.push(instance);
      apply(instance, 'loading');

      codeButton.addEventListener('click', () => {
        instance.mode = 'code';
        apply(instance, 'code');
      });
      diagramButton.addEventListener('click', () => {
        instance.mode = 'diagram';
        apply(instance, mermaidPhase({
          streaming: false,
          failed: instance.failed,
          ready: instance.ready,
          mode: 'diagram'
        }));
        if (!instance.failed && !instance.ready) activate(instance);
      });

      if (typeof IntersectionObserver === 'undefined') {
        activate(instance);
      } else {
        observer ??= new IntersectionObserver((entries) => {
          for (const entry of entries) {
            if (!entry.isIntersecting) continue;
            const found = instances.find((item) => item.wrap === entry.target);
            if (found) {
              observer?.unobserve(entry.target);
              activate(found);
            }
          }
        });
        observer.observe(wrap);
      }
    }
  }

  function teardown(): void {
    for (const instance of instances) {
      instance.disposeCss?.();
      instance.wrap.remove();
      sourceOf(instance).classList.remove('mermaid-source-hidden');
    }
    instances.length = 0;
    observer?.disconnect();
    observer = null;
  }

  if (!current.streaming) {
    // Кнопки кода и подсветка ставятся эффектами потомков: дожидаемся их, чтобы
    // обёртка встала после готового блока с кнопкой копирования.
    setTimeout(() => {
      if (!disposed && !current.streaming) setup();
    }, 0);
  }

  return {
    update(next: MermaidBlockInput): void {
      if (next.streaming !== current.streaming || next.themeKey !== current.themeKey) {
        current = next;
        teardown();
        if (!next.streaming) {
          setTimeout(() => {
            if (!disposed && !current.streaming) setup();
          }, 0);
        }
      }
    },
    destroy(): void {
      disposed = true;
      teardown();
    }
  };
}
