import { cell } from "#lib/stores/cell.svelte.js";

const PIN_PX = 48;

export interface ScrollEnv {
  raf?: (fn: () => void) => number;
  cancelRaf?: (id: number) => void;
  reducedMotion?: () => boolean;
}

export class ScrollController {
  readonly #pinned = cell(true);
  readonly #showJump = cell(false);
  readonly #el: HTMLElement;
  readonly #env: ScrollEnv;
  #last = 0;
  #lastHeight = 0;
  #lastView = 0;
  #target: number | null = null;
  #frame = 0;
  #observer: ResizeObserver | null = null;
  #onScroll: () => void;

  constructor(el: HTMLElement, env: ScrollEnv = {}) {
    this.#el = el;
    this.#env = env;
    this.#last = el.scrollTop;
    this.#remember();
    this.#onScroll = () => this.#readScroll();
    el.addEventListener("scroll", this.#onScroll, { passive: true });
    if (typeof ResizeObserver !== "undefined") {
      this.#observer = new ResizeObserver(() => this.onContentChange("resize"));
      this.#observer.observe(el);
    }
  }

  get pinned(): boolean {
    return this.#pinned.get();
  }

  get showJump(): boolean {
    return this.#showJump.get();
  }

  onContentChange(
    kind: "tail-growth" | "prepend" | "height-above" | "resize",
  ): void {
    if (kind === "prepend" || kind === "height-above") return;
    if (this.pinned) this.#toBottom();
    else if (kind !== "tail-growth") return;
    else if (this.#gap() <= PIN_PX) this.#pinned.set(true);
    else this.#showJump.set(true);
  }

  ownScroll(top: number): void {
    this.#target = top;
    this.#el.scrollTop = top;
    this.#last = this.#el.scrollTop;
    this.#remember();
  }

  /** Владелец раскрыл строку: окно перестаёт следовать за хвостом до возвращения вниз. */
  release(): void {
    this.#pinned.set(false);
    this.#cancelFrame();
  }

  jumpToBottom(): void {
    this.#pinned.set(true);
    this.#showJump.set(false);
    this.#toBottom();
  }

  destroy(): void {
    this.#el.removeEventListener("scroll", this.#onScroll);
    this.#observer?.disconnect();
    if (this.#frame) this.#env.cancelRaf?.(this.#frame);
  }

  #toBottom(): void {
    const run = (): void => {
      this.#frame = 0;
      if (!this.pinned) return;
      this.ownScroll(this.#el.scrollHeight);
    };
    if (this.#frame) return;
    const raf = this.#env.raf ?? requestAnimationFrame;
    this.#frame = raf(run);
  }

  #cancelFrame(): void {
    if (!this.#frame) return;
    const cancel = this.#env.cancelRaf ?? globalThis.cancelAnimationFrame;
    cancel?.(this.#frame);
    this.#frame = 0;
  }

  /** Геометрия запоминается только вместе с положением: так подгонка scrollTop браузером видна как сжатие, а не как уход владельца. */
  #remember(): void {
    this.#lastHeight = this.#el.scrollHeight;
    this.#lastView = this.#el.clientHeight;
  }

  #gap(): number {
    return this.#el.scrollHeight - this.#el.clientHeight - this.#el.scrollTop;
  }

  #readScroll(): void {
    const top = this.#el.scrollTop;
    const gap = this.#gap();
    const own = this.#target !== null && Math.abs(top - this.#target) <= 1;
    this.#target = null;
    // Окно стало выше (плашка над полем ввода ушла) или текст стал короче: браузер прижал scrollTop к новому пределу, а владелец не двигал ленту.
    // Новый хвост мог появиться в том же кадре, поэтому предел считается по меньшей из двух высот.
    const { scrollHeight: height, clientHeight: view } = this.#el;
    const shrank = view > this.#lastView || height < this.#lastHeight;
    const clamped = shrank && top >= Math.min(height, this.#lastHeight) - view - 1;
    this.#remember();
    const userUp = !own && !clamped && top + 1 < this.#last;
    this.#last = top;
    if (userUp) {
      this.#pinned.set(false);
      this.#showJump.set(false);
      this.#cancelFrame();
      return;
    }
    if (gap <= PIN_PX) {
      this.#pinned.set(true);
      this.#showJump.set(false);
    }
  }
}
