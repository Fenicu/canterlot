import type { BackgroundFact } from "#lib/stores/background.svelte.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import { fmt } from "#lib/format/ru.js";
import { isDocked } from "#lib/stores/requests.svelte.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import { isMainAgent } from "./present";
import { isEventOrigin } from "./hubEvent";
import type { RowKind } from "./virtual";

export type { RowKind };
export type Row = {
  key: string;
  kind: RowKind;
  entries: TimelineEntry[];
  label?: string;
};

/** Начало местных суток: подписи дней меняются раз в сутки, а не с каждым тиком часов. */
export function dayStart(ts: number): number {
  return new Date(ts).setHours(0, 0, 0, 0);
}

export interface BlockLinks {
  children(id: string): BlockModel[];
  /** Результат вызова инструмента: вывод и конец работы лежат в нём. */
  result(id: string): BlockModel | null;
  /** Родитель с учётом результатов субагента из старых строк, где вызова нет. */
  parent(block: BlockModel): string | null;
  /** Фоновая задача вызова, если он её запустил. */
  background(id: string): BackgroundFact | null;
}

const SUBAGENT_TOOLS = new Set(["Agent", "Task", "spawn_agent"]);

export function isSubagentCall(block: BlockModel): boolean {
  return block.kind === "tool_call" && SUBAGENT_TOOLS.has(block.toolName ?? "");
}

/** Связи блоков пополняются по одной записи: полный проход нужен только при пересборке окна. */
class LinksIndex {
  readonly children = new Map<string, BlockModel[]>();
  readonly adopted = new Map<string, string>();
  readonly calls = new Map<string | null, string>();

  add(entry: TimelineEntry): void {
    if (entry.kind !== "block") return;
    const block = entry.block;
    if (isSubagentCall(block)) this.calls.set(block.inputCommandId, block.id);
    const lane =
      block.parentId === null &&
      block.kind === "tool_result" &&
      !isMainAgent(block.agent)
        ? this.calls.get(block.inputCommandId)
        : undefined;
    if (lane) this.adopted.set(block.id, lane);
    const parent = block.parentId ?? lane;
    if (!parent) return;
    const list = this.children.get(parent);
    if (list) list.push(block);
    else this.children.set(parent, [block]);
  }

  remove(entry: TimelineEntry): void {
    if (entry.kind !== "block") return;
    const block = entry.block;
    const parent = block.parentId ?? this.adopted.get(block.id);
    if (parent) {
      const list = this.children.get(parent);
      const at = list?.findIndex((child) => child.id === block.id) ?? -1;
      if (at >= 0) list!.splice(at, 1);
      if (list?.length === 0) this.children.delete(parent);
    }
    this.adopted.delete(block.id);
    if (
      isSubagentCall(block) &&
      this.calls.get(block.inputCommandId) === block.id
    )
      this.calls.delete(block.inputCommandId);
  }

  snapshot(background?: ReadonlyMap<string, BackgroundFact>): BlockLinks {
    return {
      children: (id) => this.children.get(id) ?? [],
      // У субагента в детях лежат и его шаги: результат вызова тот, что вернулся основному агенту.
      result: (id) => {
        const own = this.children.get(id)?.filter(
          (child) => child.kind === "tool_result" && child.parentId === id,
        ) ?? [];
        return own.find((child) => isMainAgent(child.agent)) ?? own[0] ?? null;
      },
      parent: (block) => block.parentId ?? this.adopted.get(block.id) ?? null,
      background: (id) => background?.get(id) ?? null,
    };
  }
}

export function blockLinks(
  entries: TimelineEntry[],
  background?: ReadonlyMap<string, BackgroundFact>,
): BlockLinks {
  const index = new LinksIndex();
  for (const entry of entries) index.add(entry);
  return index.snapshot(background);
}

/** Рассуждение без текста открывать не на что: ни строки, ни отступа от него. */
export function shownInCard(block: BlockModel): boolean {
  return block.kind !== "reasoning" || block.text.trim().length > 0;
}

function stampOf(entry: TimelineEntry): number {
  if (entry.kind === "block") return entry.block.createdAt;
  if (entry.kind === "compaction") return entry.compaction.createdAt;
  if (entry.kind === "request") return 0;
  return entry.input.createdAt ?? 0;
}

/** Строки ленты накапливаются по одной записи: подпись дня считается один раз на день. */
class RowAccumulator {
  rows: Row[] = [];
  card: Row | null = null;
  day = "";
  readonly seenDays = new Set<string>();
  readonly seenRuns = new Set<string>();
  readonly #labels = new Map<number, string>();

  constructor(readonly now: number) {}

  label(stamp: number): string {
    const key = dayStart(stamp);
    let label = this.#labels.get(key);
    if (label === undefined) {
      label = fmt.dayLabel(stamp, this.now);
      this.#labels.set(key, label);
    }
    return label;
  }

  add(entry: TimelineEntry, fork = false): boolean {
    if (entry.kind === "input" && entry.input.placement !== "feed") return false;
    if (entry.kind === "request") {
      if (isDocked(entry.request)) return false;
      if (this.card) {
        this.#replaceLast(this.#withEntry(this.card, entry), fork);
        return true;
      }
      this.#push({ key: entry.key, kind: "request", entries: [entry] }, fork);
      return true;
    }
    const label = this.label(stampOf(entry));
    if (label !== this.day) {
      this.card = null;
      this.day = label;
      const base = `day:${label}`;
      const key = this.seenDays.has(base) ? `${base}:${entry.key}` : base;
      this.seenDays.add(base);
      this.#push({ key, kind: "day", entries: [], label }, fork);
    }
    if (entry.kind === "input") {
      this.card = null;
      this.#push(
        {
          key: entry.key,
          kind: isEventOrigin(entry.input.origin) ? "event" : "input",
          entries: [entry],
        },
        fork,
      );
      return true;
    }
    if (entry.kind === "compaction") {
      this.card = null;
      this.#push({ key: entry.key, kind: "compaction", entries: [entry] }, fork);
      return true;
    }
    const run = entry.block.inputCommandId;
    if (
      this.card &&
      run &&
      this.card.entries[0]?.kind === "block" &&
      this.card.entries[0].block.inputCommandId === run
    ) {
      this.#replaceLast(this.#withEntry(this.card, entry), fork);
      return true;
    }
    // Карточку узнают по ходу: её первая строка меняется, когда приходит старшая страница.
    const base = `answer:${run || entry.block.id}`;
    const card: Row = {
      key: this.seenRuns.has(base) ? `${base}:${entry.key}` : base,
      kind: "answer",
      entries: [entry],
    };
    this.seenRuns.add(base);
    this.#push(card, fork);
    return true;
  }

  /** Старейшая запись ушла из окна: убираем её строку, не пересобирая остальные. */
  drop(entry: TimelineEntry, links: BlockLinks): boolean {
    if (entry.kind === "input" && entry.input.placement !== "feed") return false;
    if (entry.kind === "request" && isDocked(entry.request)) return false;
    if (entry.kind === "block" && links.parent(entry.block) !== null) return false;
    const found = this.#locate(entry.key);
    if (!found) return false;
    const { index, row, at } = found;
    const entries = [...row.entries];
    entries.splice(at, 1);
    if (entries.length === 0) {
      const rows = [...this.rows];
      rows.splice(index, 1);
      this.rows = rows;
      if (this.card === row) this.card = null;
    } else {
      const next: Row = { ...row, entries };
      const rows = [...this.rows];
      rows[index] = next;
      this.rows = rows;
      if (this.card === row) this.card = next;
    }
    this.#trimDays();
    return true;
  }

  #locate(key: string): { index: number; row: Row; at: number } | null {
    for (let index = 0; index < this.rows.length; index++) {
      const row = this.rows[index]!;
      if (row.kind === "day") continue;
      const at = row.entries.findIndex((entry) => entry.key === key);
      if (at >= 0) return { index, row, at };
    }
    return null;
  }

  #trimDays(): void {
    let rows = this.rows;
    for (;;) {
      const first = rows[0];
      if (!first || first.kind !== "day") break;
      const next = rows[1];
      if (!next || next.kind === "day") {
        rows = rows.slice(1);
        continue;
      }
      // Запрос не задаёт день: в карточке он может стоять раньше блока.
      const head = next.entries.find((entry) => entry.kind !== "request");
      if (head && this.label(stampOf(head)) !== first.label) {
        rows = rows.slice(1);
        continue;
      }
      break;
    }
    this.rows = rows;
  }

  #withEntry(row: Row, entry: TimelineEntry): Row {
    return { ...row, entries: [...row.entries, entry] };
  }

  #push(row: Row, fork: boolean): void {
    this.rows = fork ? [...this.rows, row] : (this.rows.push(row), this.rows);
    if (row.kind === "answer") this.card = row;
  }

  #replaceLast(row: Row, fork: boolean): void {
    if (fork) {
      const rows = [...this.rows];
      rows[rows.length - 1] = row;
      this.rows = rows;
    } else this.rows[this.rows.length - 1] = row;
    this.card = row;
  }
}

export function buildRows(
  entries: TimelineEntry[],
  opts?: { laneRoot?: string; now?: number; links?: BlockLinks },
): Row[] {
  const links = opts?.links ?? blockLinks(entries);
  const source = opts?.laneRoot
    ? descendants(entries, opts.laneRoot, links)
    : entries.filter(
        (entry) => entry.kind !== "block" || links.parent(entry.block) === null,
      );
  const rows = new RowAccumulator(opts?.now ?? Date.now());
  for (const entry of source) rows.add(entry);
  return rows.rows;
}

/**
 * Строки ленты для окна: добавление в хвост патчит последнюю карточку, а старшая
 * запись, выпавшая из окна, снимает только свою строку. Полная пересборка остаётся
 * для страниц истории, смены суток и ленты субагента.
 */
export class RowsCache {
  #entries: TimelineEntry[] | null = null;
  #index = new Map<TimelineEntry, number>();
  /** Насколько номера в #index сдвинуты от начала текущего окна. */
  #offset = 0;
  #now = 0;
  #laneRoot: string | undefined;
  #background: ReadonlyMap<string, BackgroundFact> | undefined;
  #linksIndex: LinksIndex | null = null;
  #links: BlockLinks | null = null;
  #rows: RowAccumulator | null = null;

  build(
    entries: TimelineEntry[],
    opts: {
      now?: number;
      laneRoot?: string;
      background?: ReadonlyMap<string, BackgroundFact>;
    } = {},
  ): { links: BlockLinks; rows: Row[] } {
    const now = opts.now ?? Date.now();
    const laneRoot = opts.laneRoot;
    if (entries === this.#entries && now === this.#now && laneRoot === this.#laneRoot) {
      if (opts.background !== this.#background) {
        this.#background = opts.background;
        this.#links = this.#linksIndex!.snapshot(opts.background);
      }
      return { links: this.#links!, rows: this.#rows!.rows };
    }
    if (
      this.#entries !== null &&
      entries.length > 0 &&
      now === this.#now &&
      laneRoot === undefined &&
      this.#laneRoot === undefined
    ) {
      const first = this.#index.get(entries[0]!);
      if (first !== undefined) {
        const dropped = first - this.#offset;
        const appended = entries.length - (this.#entries.length - dropped);
        const tailKept =
          appended === 0 ||
          entries[entries.length - appended - 1] ===
            this.#entries[this.#entries.length - 1];
        if (dropped >= 0 && appended >= 0 && (dropped > 0 || appended > 0) && tailKept) {
          this.#append(entries, dropped, appended, opts.background);
          return { links: this.#links!, rows: this.#rows!.rows };
        }
      }
    }
    return this.#rebuild(entries, now, laneRoot, opts.background);
  }

  #append(
    entries: TimelineEntry[],
    dropped: number,
    appended: number,
    background: ReadonlyMap<string, BackgroundFact> | undefined,
  ): void {
    const old = this.#entries!;
    const linksIndex = this.#linksIndex!;
    const rows = this.#rows!;
    for (let index = 0; index < dropped; index++) {
      const entry = old[index]!;
      const links = linksIndex.snapshot(background);
      rows.drop(entry, links);
      linksIndex.remove(entry);
      this.#index.delete(entry);
    }
    const base = old.length - dropped;
    for (let index = 0; index < appended; index++) {
      const entry = entries[base + index]!;
      linksIndex.add(entry);
      const links = linksIndex.snapshot(background);
      if (entry.kind !== "block" || links.parent(entry.block) === null)
        rows.add(entry, true);
      this.#index.set(entry, base + index + this.#offset);
    }
    this.#offset += dropped;
    this.#entries = entries;
    this.#background = background;
    this.#links = linksIndex.snapshot(background);
  }

  #rebuild(
    entries: TimelineEntry[],
    now: number,
    laneRoot: string | undefined,
    background: ReadonlyMap<string, BackgroundFact> | undefined,
  ): { links: BlockLinks; rows: Row[] } {
    const linksIndex = new LinksIndex();
    for (const entry of entries) linksIndex.add(entry);
    const links = linksIndex.snapshot(background);
    const source = laneRoot
      ? descendants(entries, laneRoot, links)
      : entries.filter(
          (entry) => entry.kind !== "block" || links.parent(entry.block) === null,
        );
    const rows = new RowAccumulator(now);
    for (const entry of source) rows.add(entry);
    this.#entries = entries;
    this.#index = new Map(entries.map((entry, index) => [entry, index]));
    this.#offset = 0;
    this.#now = now;
    this.#laneRoot = laneRoot;
    this.#background = background;
    this.#linksIndex = linksIndex;
    this.#links = links;
    this.#rows = rows;
    return { links, rows: rows.rows };
  }
}

/** Последняя запись ленты: новое сообщение меняет её, а старшая страница и пересборка карточки нет. */
export function tailKey(rows: Row[]): string {
  return rows.findLast((row) => row.entries.length > 0)?.entries.at(-1)?.key ?? "";
}

function descendants(
  entries: TimelineEntry[],
  root: string,
  links: BlockLinks,
): TimelineEntry[] {
  const keep = new Set<string>();
  const walk = (id: string): void => {
    for (const child of links.children(id)) {
      keep.add(child.id);
      walk(child.id);
    }
  };
  walk(root);
  // Результат вызова показывает строка самого вызова.
  const folded = (block: BlockModel): boolean =>
    block.kind === "tool_result" &&
    block.parentId !== null &&
    block.parentId !== root;
  return entries.filter(
    (entry) =>
      entry.kind === "block" &&
      keep.has(entry.block.id) &&
      !folded(entry.block),
  );
}
