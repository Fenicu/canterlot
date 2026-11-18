import createDOMPurify from 'dompurify';
import type { DOMPurify as DOMPurifyInstance } from 'dompurify';

export type MermaidPhase = 'streaming' | 'code' | 'loading' | 'diagram' | 'error';

export interface MermaidViewInput {
  streaming: boolean;
  failed: boolean;
  ready: boolean;
  mode: 'code' | 'diagram';
}

/**
 * Незакрытый блок остаётся обычным кодом: диаграмму строим только по завершённому тексту.
 * Ошибка разбора важнее готового изображения — вместе с исходником показываем сообщение.
 */
export function mermaidPhase(input: MermaidViewInput): MermaidPhase {
  if (input.streaming) return 'streaming';
  if (input.failed) return 'error';
  if (input.mode === 'code') return 'code';
  if (input.ready) return 'diagram';
  return 'loading';
}

// Резервные значения только на случай, когда токены ещё не применены: в приложении они всегда есть.
// Имена цветов, а не литералы: визуальный язык разрешает только токены.
const FALLBACK = {
  background: 'canvas',
  surface: 'canvas',
  raised: 'canvas',
  text: 'canvastext',
  muted: 'gray',
  accent: 'highlight'
};

/** Цвета диаграммы берём из тех же токенов, что и остальной интерфейс. */
export function mermaidThemeVariables(
  read: (name: string) => string
): Record<string, string> {
  const color = (name: string, fallback: string): string => {
    const value = read(name)?.trim() ?? '';
    return /^#[0-9a-f]{3,8}$/i.test(value) ? value : fallback;
  };
  const background = color('--bg', FALLBACK.background);
  const surface = color('--surface', FALLBACK.surface);
  const raised = color('--raised', FALLBACK.raised);
  const text = color('--text', FALLBACK.text);
  // Границы и линии рисуются приглушённым текстом: сам --line на тёмной теме почти не виден.
  const muted = color('--muted', FALLBACK.muted);
  const accent = color('--accent', FALLBACK.accent);
  return {
    background,
    primaryColor: surface,
    primaryTextColor: text,
    primaryBorderColor: muted,
    secondaryColor: raised,
    tertiaryColor: background,
    lineColor: muted,
    textColor: text,
    mainBkg: surface,
    secondBkg: raised,
    border1: muted,
    border2: accent,
    nodeBorder: muted,
    clusterBkg: raised,
    clusterBorder: muted,
    edgeLabelBackground: surface,
    titleColor: text,
    labelBackground: surface,
    noteBkgColor: raised,
    noteBorderColor: muted
  };
}

function cssId(id: string): string {
  return id.replace(/[^A-Za-z0-9_-]/g, '');
}

/**
 * Mermaid кладёт стилетаг и inline-стили прямо в SVG. Наш CSP разрешает только внешние
 * таблицы, поэтому правила выносятся в строку, которую вызывающий помещает в
 * constructable stylesheet (её CSP не блокирует).
 */
export interface NaturalSize { width: number; height: number }

export function extractSvgCss(svg: string): { markup: string; css: string; natural: NaturalSize | null } {
  if (typeof DOMParser === 'undefined' || typeof XMLSerializer === 'undefined') {
    return { markup: svg, css: '', natural: null };
  }
  const doc = new DOMParser().parseFromString(svg, 'image/svg+xml');
  const root = doc.documentElement;
  if (root.nodeName.toLowerCase() !== 'svg') return { markup: '', css: '', natural: null };
  const id = cssId(root.getAttribute('id') ?? '');
  // Mermaid ставит width="100%" и max-width в инлайн-стиле. Без max-width картинка
  // растягивается на всю колонку, и подписи становятся в разы крупнее текста ленты.
  // Переносим естественный размер в атрибуты: новых инлайн-стилей при этом нет.
  const maxWidth = /max-width:\s*([\d.]+)px/i.exec(root.getAttribute('style') ?? '');
  const box = (root.getAttribute('viewBox') ?? '').trim().split(/[\s,]+/).map(Number);
  const width = Number(maxWidth?.[1] ?? box[2]);
  const height = Number(box[3]);
  const natural: NaturalSize | null =
    Number.isFinite(width) && width > 0 && Number.isFinite(height) && height > 0
      ? { width: Math.round(width), height: Math.round(height) }
      : null;
  if (natural) {
    root.setAttribute('width', String(natural.width));
    root.setAttribute('height', String(natural.height));
  }
  const rules: string[] = [];
  for (const style of Array.from(root.querySelectorAll('style'))) {
    rules.push(style.textContent ?? '');
    style.remove();
  }
  let inline = 0;
  for (const element of Array.from(root.querySelectorAll<Element>('*'))) {
    const raw = element.getAttribute('style');
    if (!raw) continue;
    const declarations = raw
      .split(';')
      .map((part) => part.trim())
      .filter(Boolean);
    element.removeAttribute('style');
    if (declarations.length === 0) continue;
    const className = `mermaid-inline-${inline++}`;
    element.classList.add(className);
    if (id) rules.push(`#${id} .${className}{${declarations.join(';')}}`);
  }
  // Браузер оставляет пустой style="" у SVG-элементов, у которых дотрагивались к .style.
  // Все объявления уже перенесены в css, поэтому атрибут можно вычистить из разметки.
  const markup = new XMLSerializer()
    .serializeToString(root)
    .replace(/\sstyle="[^"]*"/g, '');
  return { markup, css: rules.join('\n'), natural };
}

let svgPurify: DOMPurifyInstance | undefined;

/** Отдельный профиль: обычный санитайзер markdown вырезает SVG целиком. */
export function sanitizeSvg(markup: string): string {
  if (typeof window === 'undefined') return '';
  if (!svgPurify) svgPurify = createDOMPurify(window);
  return svgPurify.sanitize(markup, {
    USE_PROFILES: { svg: true, svgFilters: true },
    ADD_ATTR: [
      'id', 'class', 'viewBox', 'width', 'height', 'x', 'y', 'x1', 'x2', 'y1', 'y2',
      'rx', 'ry', 'cx', 'cy', 'r', 'd', 'transform', 'fill', 'stroke', 'stroke-width',
      'stroke-dasharray', 'stroke-dashoffset', 'stroke-linecap', 'stroke-linejoin',
      'opacity', 'fill-opacity', 'stroke-opacity', 'marker-start',
      'marker-mid', 'marker-end', 'markerWidth', 'markerHeight', 'refX', 'refY',
      'orient', 'clip-path', 'mask', 'aria-label', 'aria-hidden', 'role', 'href',
      'target', 'rel', 'font-size', 'font-family', 'text-anchor', 'dominant-baseline',
      'alignment-baseline', 'preserveAspectRatio', 'xmlns', 'xml:space'
    ],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: true
  });
}

type MermaidModule = typeof import('mermaid');

let loader: Promise<MermaidModule> | null = null;

/** Библиотека грузится только тогда, когда блок действительно попал на экран. */
export function loadMermaid(): Promise<MermaidModule> {
  if (!loader) {
    loader = import('mermaid').catch((error: unknown) => {
      loader = null;
      throw error;
    });
  }
  return loader;
}

interface Prepared {
  markup: string;
  css: string;
  natural: NaturalSize | null;
}

const MAX_CACHED = 24;
const prepared = new Map<string, Prepared>();
let sequence = 0;

export async function renderMermaidSvg(
  code: string,
  key: string,
  variables: Record<string, string>
): Promise<Prepared> {
  const cacheKey = `${key}\u0000${code}`;
  const cached = prepared.get(cacheKey);
  if (cached) {
    prepared.delete(cacheKey);
    prepared.set(cacheKey, cached);
    return cached;
  }
  const mermaid = (await loadMermaid()).default;
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    htmlLabels: false,
    suppressErrorRendering: true,
    theme: 'base',
    themeVariables: variables
  });
  const id = `mermaid-${sequence++}`;
  const { svg } = await mermaid.render(id, code);
  const extracted = extractSvgCss(svg);
  const result: Prepared = {
    markup: sanitizeSvg(extracted.markup),
    css: extracted.css,
    natural: extracted.natural
  };
  if (result.markup.includes('<svg')) {
    prepared.set(cacheKey, result);
    while (prepared.size > MAX_CACHED) {
      const oldest = prepared.keys().next().value;
      if (oldest === undefined) break;
      prepared.delete(oldest);
    }
  }
  return result;
}
