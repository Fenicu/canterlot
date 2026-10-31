import { SvelteSet } from "svelte/reactivity";

const CUT_ABOVE = 60;
const CUT_EDGE = 20;

interface Cut {
  from: string;
  to: string;
}

export interface CutView<T> {
  shown: T[];
  /** Ключ строки сразу за разрезом. */
  seam: string | null;
  hidden: number;
}

/** Что владелец раскрыл в ленте: окно виртуализации снимает строки с экрана, а раскрытое живёт дольше них. */
export class FeedFolds {
  readonly #open = new SvelteSet<string>();
  readonly #cuts = new Map<string, Cut | null>();

  isOpen(id: string): boolean {
    return this.#open.has(id);
  }

  toggle(id: string): void {
    if (!this.#open.delete(id)) this.#open.add(id);
  }

  reveal(id: string): void {
    this.#open.add(`cut:${id}`);
  }

  /** Разрез решается один раз и держится за свои строки: то, что владелец уже видел, из ленты не исчезает. */
  view<T extends { key: string }>(id: string, list: T[], live: boolean): CutView<T> {
    if (!this.#cuts.has(id) && list.length > 0) {
      const long = !live && list.length > CUT_ABOVE;
      this.#cuts.set(id, long ? { from: list[CUT_EDGE]!.key, to: list.at(-CUT_EDGE - 1)!.key } : null);
    }
    const cut = this.#cuts.get(id);
    const whole = { shown: list, seam: null, hidden: 0 };
    if (!cut || this.#open.has(`cut:${id}`)) return whole;
    const from = list.findIndex((item) => item.key === cut.from);
    const to = list.findIndex((item) => item.key === cut.to);
    if (from < 0 || to < from) return whole;
    return { shown: [...list.slice(0, from), ...list.slice(to + 1)], seam: list[to + 1]?.key ?? null, hidden: to - from + 1 };
  }
}
