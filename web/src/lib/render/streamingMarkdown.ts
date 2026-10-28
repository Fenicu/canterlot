import type { MdRenderer } from './markdown';

export interface Chunk {
  id: string;
  html: string;
}

export interface Tail {
  html: string;
  plain: boolean;
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

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

export function splitStable(
  text: string,
  fromBoundary?: number
): { boundaries: number[]; openFence: boolean } {
  const openFence = ((text.match(/^```/gm) || []).length % 2) === 1;

  const lines: { start: number; end: number; text: string; hasNewline: boolean }[] = [];
  let pos = 0;
  while (pos < text.length) {
    const nl = text.indexOf('\n', pos);
    if (nl === -1) {
      lines.push({ start: pos, end: text.length, text: text.slice(pos), hasNewline: false });
      break;
    } else {
      lines.push({ start: pos, end: nl, text: text.slice(pos, nl), hasNewline: true });
      pos = nl + 1;
    }
  }

  const boundaries: number[] = [];
  let inFence = false;
  let seenContentBefore = false;
  let sawBlankLineOutsideFence = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!line) continue;
    const rawContent = line.text.endsWith('\r') ? line.text.slice(0, -1) : line.text;
    const isFenceLine = rawContent.startsWith('```');

    if (!inFence) {
      if (rawContent.trim() === '') {
        if (seenContentBefore) {
          sawBlankLineOutsideFence = true;
        }
      } else {
        if (sawBlankLineOutsideFence) {
          const isDefiniteNotListOrContinuation = /^[^\s\d\-*+]/.test(rawContent);
          const isListMarker = /^\s*[-*+]\s/.test(rawContent) || /^\s*\d+[.)]\s/.test(rawContent);
          const isIndentedContinuation = /^\s{2,}\S/.test(rawContent);

          if (isDefiniteNotListOrContinuation) {
            boundaries.push(line.start);
            sawBlankLineOutsideFence = false;
          } else if (line.hasNewline) {
            if (!isListMarker && !isIndentedContinuation) {
              boundaries.push(line.start);
            }
            sawBlankLineOutsideFence = false;
          } else {
            if (isListMarker || isIndentedContinuation) {
              sawBlankLineOutsideFence = false;
            }
          }
        }
        seenContentBefore = true;
      }
    }

    if (isFenceLine) {
      inFence = !inFence;
    }
  }

  const resultBoundaries = fromBoundary !== undefined
    ? boundaries.filter(b => b >= fromBoundary)
    : boundaries;

  return { boundaries: resultBoundaries, openFence };
}

export class StreamingMarkdown {
  private renderer: MdRenderer;
  private chunkCache = new Map<string, string>();
  private totalRendered = 0;

  constructor(r: MdRenderer) {
    this.renderer = r;
  }

  update(text: string, final: boolean): StreamingResult {
    if (final) {
      const html = this.renderer.render(text);
      this.totalRendered++;
      return {
        chunks: [{ id: 'final', html }],
        tail: { html: '', plain: false },
        stats: { rendered: this.totalRendered, cached: 0, tailBytes: 0 }
      };
    }

    const { boundaries } = splitStable(text);
    const chunks: Chunk[] = [];
    let cachedCount = 0;

    let prev = 0;
    for (let i = 0; i < boundaries.length; i++) {
      const b = boundaries[i];
      if (b === undefined) continue;
      const chunkText = text.slice(prev, b);
      prev = b;

      let chunkHtml = this.chunkCache.get(chunkText);
      if (chunkHtml !== undefined) {
        cachedCount++;
      } else {
        chunkHtml = this.renderer.render(chunkText);
        this.chunkCache.set(chunkText, chunkHtml);
        this.totalRendered++;
      }
      chunks.push({ id: `chunk-${i}`, html: chunkHtml });
    }

    const tailText = text.slice(prev);
    const tailBytes = new TextEncoder().encode(tailText).length;

    let tail: Tail;
    if (tailText.length === 0) {
      tail = { html: '', plain: false };
    } else if (tailBytes > 8192) {
      tail = { html: escapeHtml(tailText), plain: true };
    } else {
      tail = { html: this.renderer.render(tailText), plain: false };
      this.totalRendered++;
    }

    return {
      chunks,
      tail,
      stats: {
        rendered: this.totalRendered,
        cached: cachedCount,
        tailBytes
      }
    };
  }
}
