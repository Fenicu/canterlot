export interface BatchEnvironment {
  raf?(cb: () => void): number;
  cancelRaf?(id: number): void;
  now?(): number;
  budgetMs?: number;
  visible?(): boolean;
  schedule?(fn: () => void, ms: number): () => void;
  // Оценка нужна планировщику до вызова общего reducer.
  costMs?: number;
}

export class FrameBatcher<T> {
  private queue: T[] = [];
  private cancel?: () => void;
  private disposed = false;
  private cost: number;
  private pagehide = () => this.flushNow();
  constructor(private flush: (items: T[]) => void, private env: BatchEnvironment = {}) {
    this.cost = env.costMs ?? env.budgetMs ?? 8;
    if (typeof window !== 'undefined') window.addEventListener('pagehide', this.pagehide);
  }
  push(item: T): void {
    if (this.disposed) return;
    this.queue.push(item);
    this.request();
  }
  private request(): void {
    if (this.cancel || !this.queue.length) return;
    const run = () => { this.cancel = undefined; if (!this.disposed && this.queue.length) this.slice(); };
    const visible = this.env.visible?.() ?? (typeof document === 'undefined' || document.visibilityState !== 'hidden');
    if (visible && (this.env.raf || typeof requestAnimationFrame !== 'undefined')) {
      const id = (this.env.raf ?? requestAnimationFrame)(run);
      this.cancel = () => (this.env.cancelRaf ?? globalThis.cancelAnimationFrame)?.(id);
    } else {
      if (this.env.schedule) this.cancel = this.env.schedule(run, 0);
      else { const id = setTimeout(run, 0); this.cancel = () => clearTimeout(id); }
    }
  }
  private slice(): void {
    const now = this.env.now ?? (() => performance.now());
    const budget = this.env.budgetMs ?? 8;
    const count = Math.max(1, Math.floor(budget / this.cost));
    const batch = this.queue.splice(0, count);
    const start = now();
    this.flush(batch);
    this.cost = Math.max(this.env.costMs ?? 0.05, (now() - start) / batch.length);
    this.request();
  }
  flushNow(): void {
    this.cancel?.(); this.cancel = undefined;
    if (this.queue.length) this.flush(this.queue.splice(0));
  }
  dispose(): void {
    this.disposed = true; this.flushNow();
    if (typeof window !== 'undefined') window.removeEventListener('pagehide', this.pagehide);
  }
}
