export type RowKind = 'day' | 'input' | 'event' | 'answer' | 'notice' | 'request' | 'compaction';

export function rowEstimate(kind: RowKind): number {
  if (kind === 'day') return 32;
  if (kind === 'input') return 96;
  if (kind === 'event') return 120;
  if (kind === 'notice') return 48;
  return 120;
}

export class HeightIndex {
  #estimate: (kind: RowKind) => number;
  #heights: number[] = [];
  #tree: number[] = [0];

  constructor(estimate: (kind: RowKind) => number) {
    this.#estimate = estimate;
  }

  get count(): number {
    return this.#heights.length;
  }

  setCount(n: number): void {
    const next = Math.max(0, n);
    if (next === this.#heights.length) return;
    if (next < this.#heights.length) {
      this.#heights.length = next;
      this.#rebuild();
      return;
    }
    const fill = this.#estimate('answer');
    while (this.#heights.length < next) this.#heights.push(fill);
    this.#rebuild();
  }

  set(i: number, h: number): void {
    const prev = this.#heights[i];
    if (prev === undefined || prev === h) return;
    this.#heights[i] = h;
    this.#add(i + 1, h - prev);
  }

  offset(i: number): number {
    return this.#sum(Math.max(0, Math.min(i, this.#heights.length)));
  }

  total(): number {
    return this.#sum(this.#heights.length);
  }

  indexAt(y: number): number {
    const n = this.#heights.length;
    if (n === 0) return 0;
    let lo = 0;
    let hi = n;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.offset(mid) <= y) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }

  #rebuild(): void {
    this.#tree = [0];
    for (let i = 0; i < this.#heights.length; i++) this.#tree.push(0);
    for (let i = 0; i < this.#heights.length; i++) this.#add(i + 1, this.#heights[i] ?? 0);
  }

  #add(index: number, delta: number): void {
    for (let x = index; x < this.#tree.length; x += x & -x) this.#tree[x] = (this.#tree[x] ?? 0) + delta;
  }

  #sum(count: number): number {
    let total = 0;
    for (let x = count; x > 0; x -= x & -x) total += this.#tree[x] ?? 0;
    return total;
  }
}

export interface WindowRange {
  start: number;
  end: number;
  padTop: number;
  padBottom: number;
}

export function visibleRange(ix: HeightIndex, scrollTop: number, viewport: number, overscanPx: number): WindowRange {
  const n = ix.count;
  if (n === 0) return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  const top = Math.max(0, scrollTop - overscanPx);
  const bottom = scrollTop + viewport + overscanPx;
  const start = Math.min(n - 1, ix.indexAt(top));
  let end = Math.min(n, ix.indexAt(Math.max(0, bottom)) + 1);
  if (end < start) end = start;
  return {
    start,
    end,
    padTop: ix.offset(start),
    padBottom: Math.max(0, ix.total() - ix.offset(end))
  };
}

export interface Anchor {
  key: string;
  offsetWithin: number;
}

export interface ViewAnchor {
  /** Самая глубокая строка, которую пересекает верхний край окна. */
  key: string;
  /** Её строка ленты: без неё положение восстанавливается по высотам. */
  row: string;
  top: number;
}

export function viewAnchor(el: HTMLElement): ViewAnchor | null {
  const edge = el.getBoundingClientRect().top;
  let best: HTMLElement | null = null;
  let row: HTMLElement | null = null;
  for (const node of el.querySelectorAll<HTMLElement>('[data-key]')) {
    if (best && !best.contains(node)) break;
    // Подпись дня стоит над старшей страницей, а не держит то, что под ней.
    if (node.dataset.key?.startsWith('day:') || node.getBoundingClientRect().bottom <= edge) continue;
    best = node;
    row ??= node;
  }
  if (!best || !row) return null;
  return { key: best.dataset.key ?? '', row: row.dataset.key ?? '', top: best.getBoundingClientRect().top - edge };
}

export function captureAnchor(ix: HeightIndex, keys: string[], scrollTop: number): Anchor | null {
  if (keys.length === 0) return null;
  let index = Math.min(keys.length - 1, Math.max(0, ix.indexAt(Math.max(0, scrollTop))));
  while (index < keys.length - 1 && keys[index]?.startsWith('day:')) index += 1;
  const key = keys[index];
  if (!key) return null;
  return { key, offsetWithin: scrollTop - ix.offset(index) };
}

export function restoreAnchor(ix: HeightIndex, keys: string[], anchor: Anchor): number {
  const index = keys.indexOf(anchor.key);
  if (index < 0) return 0;
  return ix.offset(index) + anchor.offsetWithin;
}
