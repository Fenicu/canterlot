import { SvelteMap } from "svelte/reactivity";

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

/** Явный выбор владельца по строке: `true` — раскрыл, `false` — закрыл, `undefined` — не касался. */
type Choice = boolean | undefined;

/** Что владелец раскрыл в ленте: окно виртуализации снимает строки с экрана, а раскрытое живёт дольше них. */
export class FeedFolds {
  /** Выбор запоминается в обе стороны: закрытый вручную блок не раскрывается настройкой по умолчанию. */
  readonly #choice = new SvelteMap<string, boolean>();
  readonly #cuts = new Map<string, Cut | null>();
  readonly #groups = new Map<string, string>();
  readonly #beforeChange: (closing: boolean, auto?: boolean) => void;

  /** `beforeChange` зовётся до правки ленты: раскрытие открывается на месте и не тянет окно к хвосту. */
  constructor(beforeChange: (closing: boolean, auto?: boolean) => void = () => undefined) {
    this.#beforeChange = beforeChange;
  }

  isOpen(id: string): boolean {
    return this.#choice.get(id) === true;
  }

  /** Выбор владельца, отличимый от «не трогали»: по нему решает настройка устройства. */
  choice(id: string): Choice {
    return this.#choice.get(id);
  }

  /** `open` — видимое сейчас состояние строки: тап по раскрытому по умолчанию блоку его закрывает. */
  toggle(id: string, open = this.isOpen(id)): void {
    this.#beforeChange(open);
    this.#choice.set(id, !open);
  }

  /** Автоматическое сворачивание: прижатая лента остаётся у хвоста, отведённая держится за якорь. */
  autoChange(closing: boolean): void {
    this.#beforeChange(closing, true);
  }

  reveal(id: string): void {
    this.#beforeChange(false);
    this.#choice.set(`cut:${id}`, true);
  }

  /** Группа растёт с двух концов: живыми шагами в хвосте и старшей страницей в голове. Имя у неё одно на всё время. */
  groupKey(members: string[], taken: Set<string>): string {
    const known = members.map((key) => this.#groups.get(key)).find((key) => key !== undefined && !taken.has(key));
    const id = known ?? `steps:${members[0]}`;
    taken.add(id);
    for (const key of members) this.#groups.set(key, id);
    return id;
  }

  /** Разрез решается один раз и держится за свои строки: то, что владелец уже видел, из ленты не исчезает. */
  view<T extends { key: string }>(id: string, list: T[], live: boolean): CutView<T> {
    if (!this.#cuts.has(id) && list.length > 0) {
      const long = !live && list.length > CUT_ABOVE;
      this.#cuts.set(id, long ? { from: list[CUT_EDGE]!.key, to: list.at(-CUT_EDGE - 1)!.key } : null);
    }
    const cut = this.#cuts.get(id);
    const whole = { shown: list, seam: null, hidden: 0 };
    if (!cut || this.isOpen(`cut:${id}`)) return whole;
    const from = list.findIndex((item) => item.key === cut.from);
    const to = list.findIndex((item) => item.key === cut.to);
    if (from < 0 || to < from) return whole;
    return { shown: [...list.slice(0, from), ...list.slice(to + 1)], seam: list[to + 1]?.key ?? null, hidden: to - from + 1 };
  }
}
