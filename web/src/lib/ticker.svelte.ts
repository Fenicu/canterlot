/**
 * Один общий маятник приложения: быстрые подписчики получают 4 Гц, медленные —
 * смену секунды. Скрытая вкладка не будит JS вовсе.
 */
const FAST_MS = 250;

export class SharedTicker {
  #now = $state(Date.now());
  #second = $state(Math.floor(Date.now() / 1000));
  #timer: number | null = null;
  #users = 0;
  readonly #doc: Document | null =
    typeof document === "undefined" ? null : document;
  readonly #onVisibility = (): void => {
    if (!this.#doc) return;
    if (this.#doc.hidden) this.#stop();
    else {
      this.#flush();
      this.#start();
    }
  };

  /** Время с шагом 250 мс: прогресс загрузок и мелкие счётчики. */
  get now(): number {
    return this.#now;
  }

  /** Смена секунды: длительности, подписи и полосы, которым чаще не нужно. */
  get second(): number {
    return this.#second;
  }

  retain(): () => void {
    this.#users += 1;
    if (this.#users === 1) {
      this.#doc?.addEventListener("visibilitychange", this.#onVisibility);
      this.#flush();
      if (!this.#doc?.hidden) this.#start();
    }
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#users -= 1;
      if (this.#users > 0) return;
      this.#stop();
      this.#doc?.removeEventListener("visibilitychange", this.#onVisibility);
    };
  }

  #flush(): void {
    const now = Date.now();
    this.#now = now;
    const second = Math.floor(now / 1000);
    if (second !== this.#second) this.#second = second;
  }

  #start(): void {
    if (this.#timer !== null || typeof window === "undefined") return;
    this.#timer = window.setInterval(() => this.#flush(), FAST_MS);
  }

  #stop(): void {
    if (this.#timer === null) return;
    window.clearInterval(this.#timer);
    this.#timer = null;
  }
}

export const ticker = new SharedTicker();
