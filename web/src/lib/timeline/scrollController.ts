import { cell } from "#lib/stores/cell.svelte.js";

const PIN_PX = 48;
/** Сколько после колеса, касания или клавиши прокрутка ещё считается рукой владельца: инерция и плавная прокрутка идут дольше самого жеста. */
const INTENT_MS = 400;
/** Прижатие на промежуточной раскладке доходит событием прокрутки не позже следующего кадра после правки ленты. */
const CHANGE_FRAMES = 2;

/**
 * Держит ленту у низа, пока владелец сам не уведёт её вверх.
 *
 * Уход вверх — это прокрутка, за которой стоит жест владельца: колесо, касание, клавиша или полоса
 * прокрутки. Браузер двигает scrollTop и сам: прижимает его к пределу на промежуточной раскладке,
 * когда хвост на миг короче, а к событию прокрутки высота уже другая. По одному положению такое
 * не отличить от владельца, поэтому подъём без жеста на изменившейся ленте возвращает её к низу.
 * Подъём без жеста на неизменной ленте остаётся прокруткой владельца: так двигают ленту
 * программы чтения экрана и тесты.
 */
export class ScrollController {
  readonly #pinned = cell(true);
  readonly #showJump = cell(false);
  readonly #el: HTMLElement;
  #last = 0;
  #lastHeight = 0;
  #lastView = 0;
  #target: number | null = null;
  /** Сколько кадров ещё считать раскладку ленты свежей после правки строк, распорок или размера. */
  #changing = 0;
  #frame = 0;
  #touching = false;
  #dragging = false;
  #intentUntil = 0;
  #observer: ResizeObserver | null = null;
  #mutations: MutationObserver | null = null;
  #off: (() => void)[] = [];

  constructor(el: HTMLElement) {
    this.#el = el;
    this.#last = el.scrollTop;
    this.#remember();
    this.#listen(el, "scroll", () => this.#readScroll());
    this.#listen(el, "wheel", () => this.#intent());
    this.#listen(el, "touchstart", () => {
      this.#touching = true;
      this.#intent();
    });
    this.#listen(el, "touchmove", () => this.#intent());
    const lift = () => {
      if (!this.#touching && !this.#dragging) return;
      this.#touching = false;
      this.#dragging = false;
      // Инерция после отпускания пальца ещё несёт ленту: это та же прокрутка владельца.
      this.#intent();
    };
    this.#listen(el, "touchend", lift);
    this.#listen(el, "touchcancel", lift);
    // Клавиши прокрутки и переход фокуса двигают ленту рукой владельца.
    this.#listen(el, "keydown", () => this.#intent());
    this.#listen(el, "focusin", () => this.#intent());
    // Нажатие по самой ленте, а не по строке в ней, — это полоса прокрутки.
    this.#listen(el, "pointerdown", (event) => {
      if (event.target !== el) return;
      this.#dragging = true;
      this.#intent();
    });
    if (typeof window !== "undefined") this.#listen(window, "pointerup", lift);
    if (typeof ResizeObserver !== "undefined") {
      this.#observer = new ResizeObserver(() => this.onContentChange("resize"));
      this.#observer.observe(el);
    }
    if (typeof MutationObserver !== "undefined" && typeof Node !== "undefined" && el instanceof Node) {
      this.#mutations = new MutationObserver(() => this.#touch());
      // Распорки окна меняют высоту через style: промежуточная раскладка бывает и без новых строк.
      // Текст не в счёт: часы хода тикают каждую секунду, а раскладку правят строки и стили.
      this.#mutations.observe(el, { childList: true, subtree: true, attributes: true, attributeFilter: ["style", "class", "hidden", "open"] });
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
    this.#touch();
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
    for (const off of this.#off.splice(0)) off();
    this.#observer?.disconnect();
    this.#mutations?.disconnect();
    if (this.#frame) cancelAnimationFrame(this.#frame);
  }

  #listen(target: EventTarget, type: string, fn: (event: Event) => void): void {
    target.addEventListener(type, fn, { passive: true });
    this.#off.push(() => target.removeEventListener(type, fn));
  }

  #now(): number {
    return typeof performance === "undefined" ? Date.now() : performance.now();
  }

  #touch(): void {
    // Счёт в кадрах, а не в миллисекундах: на медленном телефоне кадр длится дольше любого порога.
    this.#changing = CHANGE_FRAMES;
    if (this.#frame || typeof requestAnimationFrame === "undefined") return;
    const step = () => {
      this.#changing -= 1;
      this.#frame = this.#changing > 0 ? requestAnimationFrame(step) : 0;
    };
    this.#frame = requestAnimationFrame(step);
  }

  #intent(): void {
    this.#intentUntil = this.#now() + INTENT_MS;
  }

  #owner(): boolean {
    if (this.#touching || this.#dragging || this.#now() < this.#intentUntil) {
      // Пока лента едет под жестом, инерция продлевает его.
      this.#intent();
      return true;
    }
    const { scrollHeight: height, clientHeight: view } = this.#el;
    return this.#changing === 0 && height === this.#lastHeight && view === this.#lastView;
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

  /** Геометрия запоминается вместе с положением: другая высота к событию прокрутки значит, что scrollTop мог сдвинуть браузер. */
  #remember(): void {
    this.#lastHeight = this.#el.scrollHeight;
    this.#lastView = this.#el.clientHeight;
  }

  #gap(): number {
    return this.#el.scrollHeight - this.#el.clientHeight - this.#el.scrollTop;
  }

  #readScroll(): void {
    const top = this.#el.scrollTop;
    const own = this.#target !== null && Math.abs(top - this.#target) <= 1;
    this.#target = null;
    const up = !own && top + 1 < this.#last;
    const owner = up && this.#owner();
    if (up && !owner && this.pinned) {
      // Браузер прижал scrollTop на промежуточной раскладке: лента остаётся у низа.
      this.ownScroll(this.#el.scrollHeight);
      return;
    }
    this.#last = top;
    this.#remember();
    if (owner) {
      this.#pinned.set(false);
      this.#showJump.set(false);
      return;
    }
    if (this.#gap() <= PIN_PX) {
      this.#pinned.set(true);
      this.#showJump.set(false);
    }
  }
}
