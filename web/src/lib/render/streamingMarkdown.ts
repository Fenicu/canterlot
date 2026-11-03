import type { MdRenderer } from './markdown';

export interface Chunk {
  id: string;
  html: string;
}

export interface Tail {
  html: string;
  plain: boolean;
  /** Сырой текст простого хвоста: его можно дописывать в один текстовый узел без разбора HTML. */
  text?: string;
}

export interface RenderStats {
  rendered: number;
  cached: number;
  tailBytes: number;
}

export interface StreamingResult {
  chunks: Chunk[];
  tail: Tail;
  stats: RenderStats;
}

/** Готовый HTML финальных блоков: возврат к ответу не разбирает markdown заново. */
export class RenderCache {
  readonly #html = new Map<string, string>();
  #bytes = 0;

  constructor(
    readonly maxEntries = 200,
    readonly maxBytes = 4 * 1024 * 1024,
  ) {}

  get size(): number {
    return this.#html.size;
  }

  get bytes(): number {
    return this.#bytes;
  }

  get(key: string): string | undefined {
    const html = this.#html.get(key);
    if (html === undefined) return undefined;
    this.#html.delete(key);
    this.#html.set(key, html);
    return html;
  }

  set(key: string, html: string): void {
    const bytes = html.length * 2;
    if (bytes > this.maxBytes) return;
    const previous = this.#html.get(key);
    if (previous !== undefined) {
      this.#bytes -= previous.length * 2;
      this.#html.delete(key);
    }
    this.#html.set(key, html);
    this.#bytes += bytes;
    while (
      this.#html.size > this.maxEntries ||
      this.#bytes > this.maxBytes
    ) {
      const oldest = this.#html.keys().next().value;
      if (oldest === undefined) break;
      const dropped = this.#html.get(oldest)!;
      this.#html.delete(oldest);
      this.#bytes -= dropped.length * 2;
    }
  }

  clear(): void {
    this.#html.clear();
    this.#bytes = 0;
  }
}

const encoder = new TextEncoder();

function utf8Bytes(s: string): number {
  return /[\u0080-\uffff]/.test(s) ? encoder.encode(s).length : s.length;
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

interface Fence {
  marker: number;
  length: number;
  containerIndent: number;
}

// Summarise the unfinished line as characters arrive; never rescan its prefix.
class LineState {
  indent = 0;
  first = 0;
  prefix = '';
  markerLength = 0;
  markerRun = true;
  backtickInInfo = false;
  closingSpaceOnly = true;
  listFence: LineState | null = null;

  constructor(readonly start: number, readonly afterBlank = false) {}

  consume(code: number): void {
    if (this.first === 0) {
      if (code === 32 || code === 9) {
        this.indent += code === 9 ? 4 - this.indent % 4 : 1;
        return;
      }
      this.first = code;
    }
    if (this.prefix.length < 32) this.prefix += String.fromCharCode(code);
    if (this.markerRun && code === this.first && (code === 96 || code === 126)) {
      this.markerLength++;
    } else {
      this.markerRun = false;
      if (code === 96) this.backtickInInfo = true;
      if (code !== 32 && code !== 9) this.closingSpaceOnly = false;
    }
    if (this.listFence) {
      this.listFence.consume(code);
    } else if (this.prefix.length < 32) {
      const match = /^([-*+]|\d{1,9}[.)])([ \t]+)([`~])$/.exec(this.prefix);
      if (match) {
        this.listFence = new LineState(this.start);
        this.listFence.indent = this.columnAfterMarker(match);
        this.listFence.consume(code);
      }
    }
  }

  get listMarker(): boolean {
    return /^(?:[-*+]|\d{1,9}[.)])[ \t]/.test(this.prefix);
  }

  get listIndent(): number | null {
    const match = /^([-*+]|\d{1,9}[.)])([ \t]+|$)/.exec(this.prefix);
    if (!match) return null;
    const markerEnd = this.indent + match[1]!.length;
    const padding = this.columnAfterMarker(match) - markerEnd;
    return markerEnd + (padding >= 1 && padding <= 4 ? padding : 1);
  }

  private columnAfterMarker(match: RegExpExecArray): number {
    let column = this.indent + match[1]!.length;
    for (const space of match[2]!) column += space === '\t' ? 4 - column % 4 : 1;
    return column;
  }
}

class StableSplitter {
  private previous = '';
  private processed = 0;
  private line = new LineState(0);
  private fence: Fence | null = null;
  private seenContent = false;
  private afterBlank = false;
  private afterCR = false;
  private listIndents: number[] = [];
  boundaries: number[] = [];

  update(text: string): boolean {
    const reset = !text.startsWith(this.previous);
    if (reset) {
      this.processed = 0;
      this.line = new LineState(0);
      this.fence = null;
      this.seenContent = false;
      this.afterBlank = false;
      this.afterCR = false;
      this.listIndents = [];
      this.boundaries = [];
    }
    for (; this.processed < text.length; this.processed++) {
      const code = text.charCodeAt(this.processed);
      if (this.afterCR && code === 10) {
        this.afterCR = false;
        this.line = new LineState(this.processed + 1, this.afterBlank);
        continue;
      }
      this.afterCR = code === 13;
      if (code === 10 || code === 13) {
        this.updateLists();
        this.considerBoundary(true);
        if (!this.fence) {
          if (this.line.first === 0) {
            if (this.seenContent) this.afterBlank = true;
          } else {
            this.seenContent = true;
          }
        }
        this.fence = this.fenceAfterLine();
        this.line = new LineState(this.processed + 1, this.afterBlank);
      } else {
        this.line.consume(code);
        this.considerBoundary(false);
      }
    }
    this.previous = text;
    return reset;
  }

  private considerBoundary(complete: boolean): void {
    if (this.fence || !this.afterBlank || this.line.first === 0) return;
    // List continuations need at least two columns; a single tab supplies four.
    if (this.line.indent >= 2 || this.line.listMarker) {
      this.afterBlank = false;
      return;
    }
    const first = this.line.first;
    const couldBeList = (first >= 48 && first <= 57) || first === 45 || first === 42 || first === 43;
    if (complete || (!couldBeList && this.line.indent === 0)) {
      this.boundaries.push(this.line.start);
      this.afterBlank = false;
      this.listIndents = [];
    }
  }

  private containerAt(indent: number): number {
    let lo = 0;
    let hi = this.listIndents.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.listIndents[mid]! <= indent) lo = mid + 1;
      else hi = mid;
    }
    return lo === 0 ? 0 : this.listIndents[lo - 1]!;
  }

  private updateLists(): void {
    const line = this.line;
    if (line.first === 0) return;
    if (this.fence && line.indent >= this.fence.containerIndent) return;
    const listIndent = line.listIndent;
    if (this.fence || line.afterBlank || listIndent !== null || line.markerLength >= 3) {
      while (this.listIndents.length && this.listIndents[this.listIndents.length - 1]! > line.indent) {
        this.listIndents.pop();
      }
    }
    if (listIndent !== null && line.indent - this.containerAt(line.indent) < 4) {
      this.listIndents.push(listIndent);
    }
  }

  private fenceAfterLine(): Fence | null {
    const raw = this.line;
    let fence = this.fence;
    // Outdenting ends the list's fence before this line is interpreted anew.
    if (fence && raw.first !== 0 && raw.indent < fence.containerIndent) fence = null;
    const line = !fence && raw.listFence ? raw.listFence : raw;
    const containerIndent = fence?.containerIndent ??
      (line === raw ? this.containerAt(line.indent) : raw.listIndent!);
    if (line.indent - containerIndent >= 4 || line.markerLength < 3) return fence;
    if (fence) {
      return line.first === fence.marker && line.markerLength >= fence.length && line.closingSpaceOnly
        ? null : fence;
    }
    if (line.first === 96 && line.backtickInInfo) return null;
    return { marker: line.first, length: line.markerLength, containerIndent };
  }

  get openFence(): boolean {
    // EOL may close a fence for this result, but a later append can invalidate
    // that closing line. Only newline-terminated lines commit the fence state.
    return this.fenceAfterLine() !== null;
  }
}

const standaloneSplitter = new StableSplitter();

export function splitStable(
  text: string,
  fromBoundary?: number
): { boundaries: number[]; openFence: boolean } {
  standaloneSplitter.update(text);
  const boundaries = standaloneSplitter.boundaries;
  let lo = 0;
  let hi = boundaries.length;
  if (fromBoundary !== undefined) {
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (boundaries[mid]! < fromBoundary) lo = mid + 1;
      else hi = mid;
    }
  }
  return { boundaries: boundaries.slice(lo), openFence: standaloneSplitter.openFence };
}

export const finalHtmlCache = new RenderCache();

export class StreamingMarkdown {
  private renderer: MdRenderer;
  private chunkCache = new Map<string, string>();
  private totalRendered = 0;
  private splitter = new StableSplitter();
  private chunks: Chunk[] = [];
  private chunkEnd = 0;
  private closedBoundaries = 0;

  constructor(
    r: MdRenderer,
    private cache: RenderCache | null = finalHtmlCache,
  ) {
    this.renderer = r;
  }

  update(text: string, final: boolean, key?: string): StreamingResult {
    if (final) {
      const cached = key ? this.cache?.get(key) : undefined;
      if (cached !== undefined) {
        return {
          chunks: [{ id: 'final', html: cached }],
          tail: { html: '', plain: false },
          stats: { rendered: this.totalRendered, cached: 1, tailBytes: 0 }
        };
      }
      const html = this.renderer.render(text);
      this.totalRendered++;
      if (key) this.cache?.set(key, html);
      return {
        chunks: [{ id: 'final', html }],
        tail: { html: '', plain: false },
        stats: { rendered: this.totalRendered, cached: 0, tailBytes: 0 }
      };
    }

    if (this.splitter.update(text)) {
      this.chunks = [];
      this.chunkEnd = 0;
      this.closedBoundaries = 0;
      this.chunkCache.clear();
    }
    const boundaries = this.splitter.boundaries;
    let cachedCount = this.chunks.length;
    while (this.closedBoundaries < boundaries.length) {
      const boundary = boundaries[this.closedBoundaries++]!;
      const chunkText = text.slice(this.chunkEnd, boundary);
      this.chunkEnd = boundary;
      let chunkHtml = this.chunkCache.get(chunkText);
      if (chunkHtml !== undefined) {
        cachedCount++;
      } else {
        chunkHtml = this.renderer.render(chunkText);
        this.chunkCache.set(chunkText, chunkHtml);
        this.totalRendered++;
      }
      this.chunks.push({ id: `chunk-${this.chunks.length}`, html: chunkHtml });
    }

    const tailText = text.slice(this.chunkEnd);
    const tailBytes = utf8Bytes(tailText);

    let tail: Tail;
    if (tailText.length === 0) {
      tail = { html: '', plain: false };
    } else if (tailBytes > 8192) {
      tail = {
        plain: true,
        text: tailText,
        get html() {
          return escapeHtml(tailText);
        }
      };
    } else {
      tail = { html: this.renderer.render(tailText), plain: false };
      this.totalRendered++;
    }

    return {
      chunks: this.chunks,
      tail,
      stats: {
        rendered: this.totalRendered,
        cached: cachedCount,
        tailBytes
      }
    };
  }
}
