import { cell } from "#lib/stores/cell.svelte.js";

const PIN_PX = 48;

export class ScrollController {
  readonly #pinned = cell(true);
  readonly #showJump = cell(false);
  readonly #el: HTMLElement;
  #last = 0;
  #lastHeight = 0;
  #lastView = 0;
  #target: number | null = null;
  #observer: ResizeObserver | null = null;
  #onScroll: () => void;

  constructor(el: HTMLElement) {
    this.#el = el;
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
    if (this.pinned) return;
    // Хвост вырос, а окно уже не прижато: показываем кнопку возврата.
    if (kind !== "tail-growth") return;
    if (this.#gap() <= PIN_PX) this.#pinned.set(true);
    else this.#showJump.set(true);
  }

  ownScroll(top: number): void {
    this.#target = top;
    this.#el.scrollTop = top;
    this.#last = this.#el.scrollTop;
    this.#remember();
  }

  /**
   * Лента только что разложена посреди своей перестройки. Если браузер на этой раскладке прижал
   * scrollTop к новому, меньшему пределу, это не владелец: подъём ровно к низу ленты прокруткой вверх
   * не бывает. Без этого высота успевала снова вырасти до события прокрутки, и прижатие читалось как уход вверх.
   */
  noteLayout(): void {
    const top = this.#el.scrollTop;
    if (top >= this.#last) return;
    if (top < this.#el.scrollHeight - this.#el.clientHeight - 1) return;
    this.#last = top;
    this.#remember();
  }

  /** Владелец раскрыл строку: окно перестаёт следовать за хвостом до возвращения вниз. */
  release(): void {
    this.#pinned.set(false);
  }

  jumpToBottom(): void {
    this.#pinned.set(true);
    this.#showJump.set(false);
    this.#toBottom(true);
  }

  destroy(): void {
    this.#el.removeEventListener("scroll", this.#onScroll);
    this.#observer?.disconnect();
  }

  #toBottom(explicit = false): void {
    // Прокрутка идёт в том же кадре, что и рост текста: иначе кадр рисуется
    // с подвалом ниже низа, а следующий кадр рывком возвращает его на место.
    if (!this.pinned) return;
    // Событие scroll могло ещё не дойти: владелец уже сдвинул ленту, и решение
    // «прижато» нельзя брать из прошлого кадра, иначе подвал уедет к низу.
    if (!explicit && this.#el.scrollTop !== this.#last) this.#readScroll();
    if (!this.pinned) return;
    this.ownScroll(this.#el.scrollHeight);
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
      return;
    }
    if (gap <= PIN_PX) {
      this.#pinned.set(true);
      this.#showJump.set(false);
    }
  }
}
