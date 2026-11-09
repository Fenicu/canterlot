import type { BackgroundFact } from "#lib/stores/background.svelte.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import { fmt } from "#lib/format/ru.js";
import { isDocked } from "#lib/stores/requests.svelte.js";
import type { RequestItem } from "#lib/stores/requests.svelte.js";
import type { InputView } from "#lib/stores/inputs.js";
import type { CompactionModel, TimelineEntry } from "#lib/stores/timeline.js";
import { isMainAgent } from "./present";
import { isEventOrigin } from "./hubEvent";
import { toolId } from "./cards/toolId";
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
  /** Блок-носитель файла, который воркер записал для этого вызова `share_file`. */
  carrier(id: string): BlockModel | null;
}

const SUBAGENT_TOOLS = new Set(["Agent", "Task", "spawn_agent"]);

export function isSubagentCall(block: BlockModel): boolean {
  return block.kind === "tool_call" && SUBAGENT_TOOLS.has(block.toolName ?? "");
}

/**
 * Блок-носитель файла `share_file`: его пишет воркер после загрузки, чтобы хаб
 * привязал вложение. Модели у него нет, имя инструмента голое; вызов модели
 * (`mcp__canterlot__share_file`, `canterlot.share_file`, `canterlot_share_file`)
 * идёт отдельным блоком с моделью.
 */
export function isShareCarrier(block: BlockModel): boolean {
  return (
    block.kind === "tool_call" &&
    block.toolName === "share_file" &&
    block.model === "" &&
    block.parentId === null
  );
}

function isShareCall(block: BlockModel): boolean {
  return (
    block.kind === "tool_call" &&
    !isShareCarrier(block) &&
    toolId(block.toolName) === "canterlot.share_file"
  );
}

/** Подготовка и передача файла укладываются в полчаса: дальше носитель не ищет вызов. */
const SHARE_PAIR_MS = 30 * 60_000;

/** Подпись, которую воркер даст носителю: заголовок, иначе имя файла из пути. */
function shareLabel(call: BlockModel): string {
  try {
    const input: unknown = JSON.parse(call.text);
    if (!input || typeof input !== "object") return "";
    const { title, path } = input as { title?: unknown; path?: unknown };
    if (typeof title === "string" && title) return title;
    if (typeof path === "string") return path.slice(path.lastIndexOf("/") + 1);
  } catch {
    // Вход ещё пишется или не JSON: подпись не сравнить, остаётся порядок.
  }
  return "";
}

function sharePair(call: BlockModel, carrier: BlockModel): boolean {
  if (call.inputCommandId && carrier.inputCommandId && call.inputCommandId !== carrier.inputCommandId)
    return false;
  return Math.abs(carrier.createdAt - call.createdAt) <= SHARE_PAIR_MS;
}

/** Связи блоков пополняются по одной записи: полный проход нужен только при пересборке окна. */
class LinksIndex {
  readonly children = new Map<string, BlockModel[]>();
  readonly adopted = new Map<string, string>();
  readonly calls = new Map<string | null, string>();
  /** Вызовы `share_file` и носители файлов по порядку прихода. */
  readonly shareCalls: BlockModel[] = [];
  readonly carriers: BlockModel[] = [];
  /** Вызов → его носитель и носитель → вызов: носитель не встаёт в ленту отдельно. */
  readonly carrierOf = new Map<string, BlockModel>();
  readonly carriedBy = new Map<string, string>();

  /**
   * Добавляет запись. `true` — запись перевязала уже показанный блок (носитель пришёл
   * раньше вызова): ленту надо пересобрать.
   */
  add(entry: TimelineEntry): boolean {
    if (entry.kind !== "block") return false;
    const block = entry.block;
    const relinked = this.#pairShare(block);
    if (isSubagentCall(block)) this.calls.set(block.inputCommandId, block.id);
    const lane =
      block.parentId === null &&
      block.kind === "tool_result" &&
      !isMainAgent(block.agent)
        ? this.calls.get(block.inputCommandId)
        : undefined;
    if (lane) this.adopted.set(block.id, lane);
    const parent = block.parentId ?? lane;
    if (!parent) return relinked;
    const list = this.children.get(parent);
    // Новый массив, чтобы строка субагента увидела добавленный шаг.
    this.children.set(parent, list ? [...list, block] : [block]);
    return relinked;
  }

  /**
   * Один `share_file` — одна карточка: носитель файла прячется за вызов модели.
   * Пара ищется в том же ходе и рядом по времени; при нескольких файлах сначала по
   * подписи (заголовок или имя файла), иначе по порядку — воркер грузит файлы по очереди.
   */
  #pairShare(block: BlockModel): boolean {
    if (isShareCarrier(block)) {
      this.carriers.push(block);
      const free = this.shareCalls.filter((call) => !this.carrierOf.has(call.id) && sharePair(call, block));
      const call = free.find((item) => shareLabel(item) === block.summary) ?? free[0];
      if (call) this.#link(call, block);
      return false;
    }
    if (!isShareCall(block)) return false;
    this.shareCalls.push(block);
    // Носитель раньше вызова бывает только у нового воркера, который пишет ход; носитель
    // без хода мог прийти от вызова со старшей, ещё не загруженной страницы.
    const free = this.carriers.filter(
      (carrier) =>
        !this.carriedBy.has(carrier.id) &&
        carrier.inputCommandId !== null &&
        carrier.inputCommandId === block.inputCommandId &&
        sharePair(block, carrier),
    );
    const label = shareLabel(block);
    const carrier = free.find((item) => item.summary === label) ?? free[0];
    if (!carrier) return false;
    this.#link(block, carrier);
    return true;
  }

  #link(call: BlockModel, carrier: BlockModel): void {
    this.carrierOf.set(call.id, carrier);
    this.carriedBy.set(carrier.id, call.id);
  }

  /** Снимает запись. `true` — её носитель остался без вызова и должен встать в ленту. */
  remove(entry: TimelineEntry): boolean {
    if (entry.kind !== "block") return false;
    const block = entry.block;
    let orphaned = false;
    const carrier = this.carrierOf.get(block.id);
    if (carrier) {
      this.carrierOf.delete(block.id);
      this.carriedBy.delete(carrier.id);
      orphaned = true;
    }
    const call = this.carriedBy.get(block.id);
    if (call) {
      this.carriedBy.delete(block.id);
      this.carrierOf.delete(call);
    }
    const shareAt = this.shareCalls.indexOf(block);
    if (shareAt >= 0) this.shareCalls.splice(shareAt, 1);
    const carrierAt = this.carriers.indexOf(block);
    if (carrierAt >= 0) this.carriers.splice(carrierAt, 1);
    const parent = block.parentId ?? this.adopted.get(block.id);
    if (parent) {
      const list = this.children.get(parent);
      const next = list?.filter((child) => child.id !== block.id) ?? [];
      if (next.length > 0) this.children.set(parent, next);
      else this.children.delete(parent);
    }
    this.adopted.delete(block.id);
    if (
      isSubagentCall(block) &&
      this.calls.get(block.inputCommandId) === block.id
    )
      this.calls.delete(block.inputCommandId);
    return orphaned;
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
      parent: (block) =>
        block.parentId ?? this.adopted.get(block.id) ?? this.carriedBy.get(block.id) ?? null,
      background: (id) => background?.get(id) ?? null,
      carrier: (id) => this.carrierOf.get(id) ?? null,
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

function modelOf(entry: TimelineEntry): BlockModel | RequestItem | InputView | CompactionModel {
  switch (entry.kind) {
    case "block":
      return entry.block;
    case "request":
      return entry.request;
    case "input":
      return entry.input;
    default:
      return entry.compaction;
  }
}

/**
 * Поля записи, от которых зависит состав строк: видимость в ленте, карточка хода,
 * вид строки и подпись дня. Чтение этих полей подписывает ленту на их обновление.
 */
function shapeOf(entry: TimelineEntry): string {
  if (entry.kind === "block") {
    const block = entry.block;
    return [
      "b",
      entry.seq,
      block.kind,
      block.createdAt,
      block.parentId,
      block.inputCommandId,
      block.toolName,
      block.agent,
    ].join("|");
  }
  if (entry.kind === "request") {
    return ["r", entry.seq, entry.request.state].join("|");
  }
  if (entry.kind === "input") {
    const input = entry.input;
    return [
      "i",
      entry.seq,
      input.placement,
      isEventOrigin(input.origin),
      input.createdAt,
      input.state,
    ].join("|");
  }
  return ["c", entry.seq, entry.compaction.createdAt, entry.compaction.ok].join("|");
}

type EntryRecord = {
  entry: TimelineEntry;
  model: BlockModel | RequestItem | InputView | CompactionModel;
  shape: string;
};

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

  /** Можно ли снять запись, не меняя вид соседних строк: запросы и начала карточек — нет. */
  canDrop(entry: TimelineEntry, links: BlockLinks): boolean {
    if (entry.kind === "input" && entry.input.placement !== "feed") return true;
    if (entry.kind === "request" && isDocked(entry.request)) return true;
    if (entry.kind === "block" && links.parent(entry.block) !== null) return true;
    const found = this.#locate(entry.key);
    if (!found) return true;
    const { row, at } = found;
    if (row.entries.some((item) => item.kind === "request")) return false;
    if (row.entries.length > 1 && at === 0) return false;
    if (
      row.entries.length > 1 &&
      row.entries.every((item, index) => index === at || item.kind === "request")
    )
      return false;
    return true;
  }

  /** Старейшая запись ушла из окна: убираем её строку, не пересобирая остальные. */
  drop(entry: TimelineEntry, links: BlockLinks): boolean {
    if (!this.canDrop(entry, links)) return false;
    if (entry.kind === "input" && entry.input.placement !== "feed") return true;
    if (entry.kind === "request" && isDocked(entry.request)) return true;
    if (entry.kind === "block" && links.parent(entry.block) !== null) return true;
    const found = this.#locate(entry.key);
    if (!found) return true;
    const { index, row, at } = found;
    const entries = [...row.entries];
    entries.splice(at, 1);
    if (entries.length === 0) {
      const rows = [...this.rows];
      rows.splice(index, 1);
      this.rows = rows;
      if (this.card === row) this.card = null;
      // Снятая карточка больше не занимает свой ход: новая с тем же ходом получит базовый ключ.
      if (row.kind === "answer") {
        const first = row.entries[0];
        if (first?.kind === "block") this.#forgetRun(first.block);
      }
      if (row.kind === "day" && row.label) this.seenDays.delete(`day:${row.label}`);
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

  #forgetRun(first: BlockModel): void {
    const base = `answer:${first.inputCommandId || first.id}`;
    const still = this.rows.find(
      (row) =>
        row.kind === "answer" &&
        row.entries[0]?.kind === "block" &&
        `answer:${row.entries[0].block.inputCommandId || row.entries[0].block.id}` === base,
    );
    if (!still) {
      this.seenRuns.delete(base);
      return;
    }
    // Первая карточка того же хода получила ключ с суффиксом: без старшей он снова базовый.
    if (still.key !== base) {
      const index = this.rows.indexOf(still);
      const next: Row = { ...still, key: base };
      const rows = [...this.rows];
      rows[index] = next;
      this.rows = rows;
      if (this.card === still) this.card = next;
    }
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
    let touched = false;
    for (;;) {
      const first = rows[0];
      if (!first || first.kind !== "day") break;
      let at = 1;
      while (at < rows.length) {
        const row = rows[at]!;
        if (row.kind === "day") break;
        if (row.entries.some((entry) => entry.kind !== "request")) break;
        at++;
      }
      if (at >= rows.length) {
        this.#forgetDay(rows, 1, first.label);
        rows = rows.slice(1);
        touched = true;
        continue;
      }
      const target = rows[at]!;
      if (target.kind === "day") {
        this.#forgetDay(rows, 1, first.label);
        rows = rows.slice(1);
        touched = true;
        continue;
      }
      // Запрос не задаёт день: подпись встаёт перед первым блоком, а не перед запросами.
      const head = target.entries.find((entry) => entry.kind !== "request")!;
      const label = this.label(stampOf(head));
      if (at === 1 && label === first.label) break;
      this.#forgetDay(rows, 1, first.label);
      const base = `day:${label}`;
      const key = this.seenDays.has(base) ? `${base}:${head.key}` : base;
      this.seenDays.add(base);
      const moved: Row = { ...first, key, label };
      const without = rows.slice(1);
      rows = [
        ...without.slice(0, at - 1),
        moved,
        ...without.slice(at - 1),
      ];
      touched = true;
      break;
    }
    this.rows = rows;
    if (rows.length === 0) {
      this.day = "";
      this.card = null;
      this.seenDays.clear();
      this.seenRuns.clear();
      return;
    }
    if (!touched) return;
    // День последней обработанной записи: он же подпись для следующего добавления.
    for (let index = rows.length - 1; index >= 0; index--) {
      const row = rows[index]!;
      if (row.kind === "day") {
        this.day = row.label ?? "";
        break;
      }
      const head = row.entries.find((entry) => entry.kind !== "request");
      if (head) {
        this.day = this.label(stampOf(head));
        break;
      }
    }
  }

  #forgetDay(rows: Row[], from: number, label: string | undefined): void {
    if (!label) return;
    const base = `day:${label}`;
    if (rows.slice(from).some((row) => row.kind === "day" && row.label === label)) return;
    this.seenDays.delete(base);
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
 * Строки ленты для окна. Добавление в хвост и срез головы идут одним проходом по
 * изменившимся записям. Любое обновление уже лежащей записи пересобирает окно, но
 * неизменившиеся строки сохраняют свои объекты: перерисовываются только затронутые.
 */
export class RowsCache {
  #entries: TimelineEntry[] | null = null;
  #records = new Map<string, EntryRecord>();
  #now = 0;
  #laneRoot: string | undefined;
  #background: ReadonlyMap<string, BackgroundFact> | undefined;
  #linksIndex: LinksIndex | null = null;
  #links: BlockLinks | null = null;
  #rows: RowAccumulator | null = null;
  #rowsByKey = new Map<string, Row>();

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
    const { records, changed } = this.#scan(entries);
    const same = this.#entries === entries && now === this.#now && laneRoot === this.#laneRoot;

    if (same && changed.length === 0) {
      if (opts.background !== this.#background) {
        this.#background = opts.background;
        this.#links = this.#linksIndex!.snapshot(opts.background);
      }
      return { links: this.#links!, rows: this.#rows!.rows };
    }

    if (
      this.#entries !== null &&
      now === this.#now &&
      laneRoot === undefined &&
      this.#laneRoot === undefined
    ) {
      const shift = this.#shift(entries, records);
      if (shift && (shift.dropped > 0 || shift.appended.length > 0)) {
        if (this.#slide(entries, records, shift, opts.background))
          return { links: this.#links!, rows: this.#rows!.rows };
      }
    }

    return this.#rebuild(entries, records, now, laneRoot, opts.background);
  }

  #scan(entries: TimelineEntry[]): { records: Map<string, EntryRecord>; changed: number[] } {
    const records = new Map<string, EntryRecord>();
    const changed: number[] = [];
    let index = 0;
    for (const entry of entries) {
      const shape = shapeOf(entry);
      const model = modelOf(entry);
      records.set(entry.key, { entry, model, shape });
      const old = this.#records.get(entry.key);
      if (!old || old.shape !== shape || old.model !== model || old.entry.seq !== entry.seq)
        changed.push(index);
      index++;
    }
    for (const key of this.#records.keys())
      if (!records.has(key)) changed.push(-1);
    return { records, changed };
  }

  /**
   * Хвост вырос, голова срезана, а всё, что осталось на месте, не менялось.
   * Любое сомнение — полная пересборка.
   */
  #shift(
    entries: TimelineEntry[],
    records: Map<string, EntryRecord>,
  ): { dropped: number; appended: TimelineEntry[] } | null {
    const old = this.#entries!;
    let dropped = 0;
    while (dropped < old.length && !records.has(old[dropped]!.key)) dropped++;
    for (let index = dropped; index < old.length; index++)
      if (!records.has(old[index]!.key)) return null;
    const kept = old.length - dropped;
    if (kept > entries.length) return null;
    for (let index = 0; index < kept; index++) {
      const before = old[dropped + index]!;
      const after = entries[index]!;
      const record = records.get(after.key);
      const previous = this.#records.get(before.key);
      if (
        !record ||
        !previous ||
        before.key !== after.key ||
        record.shape !== previous.shape ||
        record.model !== previous.model ||
        before.seq !== after.seq
      )
        return null;
    }
    const appended = entries.slice(kept);
    return { dropped, appended };
  }

  #slide(
    entries: TimelineEntry[],
    records: Map<string, EntryRecord>,
    shift: { dropped: number; appended: TimelineEntry[] },
    background: ReadonlyMap<string, BackgroundFact> | undefined,
  ): boolean {
    const old = this.#entries!;
    const linksIndex = this.#linksIndex!;
    const rows = this.#rows!;
    for (let index = 0; index < shift.dropped; index++) {
      const entry = old[index]!;
      const links = linksIndex.snapshot(background);
      if (!rows.drop(entry, links)) return false;
      // Носитель файла остался без вызова: он встаёт в ленту, это пересборка.
      if (linksIndex.remove(entry)) return false;
    }
    for (const entry of shift.appended) {
      // Вызов пришёл после своего носителя, который уже стоит в ленте.
      if (linksIndex.add(entry)) return false;
      const links = linksIndex.snapshot(background);
      if (entry.kind !== "block" || links.parent(entry.block) === null)
        rows.add(entry, true);
    }
    this.#entries = entries;
    this.#records = records;
    this.#background = background;
    this.#links = linksIndex.snapshot(background);
    this.#rowsByKey = new Map(rows.rows.map((row) => [row.key, row]));
    return true;
  }

  #rebuild(
    entries: TimelineEntry[],
    records: Map<string, EntryRecord>,
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
    const accumulator = new RowAccumulator(now);
    for (const entry of source) accumulator.add(entry);
    // Объекты неизменившихся строк переживают пересборку: окно не перерисовывает их заново.
    const previous = this.#rowsByKey;
    const previousRecords = this.#records;
    const rows = accumulator.rows.map((row) => {
      const before = previous.get(row.key);
      if (
        before &&
        before.kind === row.kind &&
        (before.label ?? null) === (row.label ?? null) &&
        this.#sameEntries(before, row, previousRecords, records)
      )
        return before;
      return row;
    });
    accumulator.rows = rows;
    this.#entries = entries;
    this.#records = records;
    this.#now = now;
    this.#laneRoot = laneRoot;
    this.#background = background;
    this.#linksIndex = linksIndex;
    this.#links = links;
    this.#rows = accumulator;
    this.#rowsByKey = new Map(rows.map((row) => [row.key, row]));
    return { links, rows };
  }

  #sameEntries(
    before: Row,
    after: Row,
    previous: Map<string, EntryRecord>,
    current: Map<string, EntryRecord>,
  ): boolean {
    if (before.entries.length !== after.entries.length) return false;
    for (let index = 0; index < after.entries.length; index++) {
      const oldEntry = before.entries[index]!;
      const newEntry = after.entries[index]!;
      if (oldEntry.key !== newEntry.key) return false;
      const old = previous.get(oldEntry.key);
      const next = current.get(newEntry.key);
      if (
        !old ||
        !next ||
        old.shape !== next.shape ||
        old.model !== next.model ||
        oldEntry.seq !== newEntry.seq
      )
        return false;
    }
    return true;
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
