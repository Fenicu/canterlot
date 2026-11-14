import { clock } from '#lib/clock.js';

export const STALE_AFTER_MS = 5_000;

export interface StalenessDeps {
  now(): number;
  /** Возвращает отмену запланированного вызова. */
  schedule(run: () => void, ms: number): () => void;
}

/**
 * Пять секунд после появления баннера связи картина ещё может быть правдой:
 * за это время короткий обрыв успевает закрыться. Дальше часы замирают, и весь
 * экран честно показывает снимок: одно место решает это для всех таймеров.
 */
export class ConnectionStaleness {
  stale = $state(false);
  #frozenAt = 0;
  #cancel: (() => void) | null = null;
  #down = false;

  constructor(private deps: StalenessDeps) {}

  /** Единый источник «сейчас» для живых подписей: пока связи нет, он стоит. */
  now(): number {
    return this.stale ? this.#frozenAt : this.deps.now();
  }

  get frozenAt(): number | null {
    return this.stale ? this.#frozenAt : null;
  }

  /** `down` — баннер связи уже виден. Повторные вызовы не сдвигают срок. */
  update(down: boolean): void {
    this.#down = down;
    if (!down) {
      this.#cancel?.();
      this.#cancel = null;
      this.stale = false;
      return;
    }
    if (this.stale || this.#cancel) return;
    this.#cancel = this.deps.schedule(() => {
      this.#cancel = null;
      if (!this.#down) return;
      this.#frozenAt = this.deps.now();
      this.stale = true;
    }, STALE_AFTER_MS);
  }
}

export const connectionStale = new ConnectionStaleness({
  now: () => clock.serverNow(),
  schedule: (run, ms) => {
    const timer = setTimeout(run, ms);
    return () => clearTimeout(timer);
  }
});
