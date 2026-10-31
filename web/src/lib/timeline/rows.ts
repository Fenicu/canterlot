import type { BlockModel } from "#lib/stores/blocks.js";
import { fmt } from "#lib/format/ru.js";
import { isDocked } from "#lib/stores/requests.svelte.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import { isMainAgent } from "./present";
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
}

const SUBAGENT_TOOLS = new Set(["Agent", "Task"]);

export function isSubagentCall(block: BlockModel): boolean {
  return block.kind === "tool_call" && SUBAGENT_TOOLS.has(block.toolName ?? "");
}

export function blockLinks(entries: TimelineEntry[]): BlockLinks {
  const children = new Map<string, BlockModel[]>();
  const adopted = new Map<string, string>();
  const calls = new Map<string | null, string>();
  for (const entry of entries) {
    if (entry.kind !== "block") continue;
    const block = entry.block;
    if (isSubagentCall(block)) calls.set(block.inputCommandId, block.id);
    const lane =
      block.parentId === null &&
      block.kind === "tool_result" &&
      !isMainAgent(block.agent)
        ? calls.get(block.inputCommandId)
        : undefined;
    if (lane) adopted.set(block.id, lane);
    const parent = block.parentId ?? lane;
    if (!parent) continue;
    const list = children.get(parent);
    if (list) list.push(block);
    else children.set(parent, [block]);
  }
  return {
    children: (id) => children.get(id) ?? [],
    result: (id) =>
      children.get(id)?.find((child) => child.kind === "tool_result") ?? null,
    parent: (block) => block.parentId ?? adopted.get(block.id) ?? null,
  };
}

/** Рассуждение без текста открывать не на что: ни строки, ни отступа от него. */
export function shownInCard(block: BlockModel): boolean {
  return block.kind !== "reasoning" || block.text.trim().length > 0;
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
  const rows: Row[] = [];
  let card: Row | null = null;
  let day = "";
  const seenDays = new Set<string>();
  const seenRuns = new Set<string>();
  const now = opts?.now ?? Date.now();
  for (const entry of source) {
    if (entry.kind === "input" && entry.input.placement !== "feed") continue;
    if (entry.kind === "request") {
      if (isDocked(entry.request)) continue;
      if (card) card.entries.push(entry);
      else rows.push({ key: entry.key, kind: "request", entries: [entry] });
      continue;
    }
    const stamp =
      entry.kind === "block"
        ? entry.block.createdAt
        : (entry.input.createdAt ?? 0);
    const label = fmt.dayLabel(stamp, now);
    if (label !== day) {
      card = null;
      day = label;
      const base = `day:${label}`;
      const key = seenDays.has(base) ? `${base}:${entry.key}` : base;
      seenDays.add(base);
      rows.push({ key, kind: "day", entries: [], label });
    }
    if (entry.kind === "input") {
      card = null;
      rows.push({ key: entry.key, kind: "input", entries: [entry] });
      continue;
    }
    const run = entry.block.inputCommandId;
    if (
      card &&
      run &&
      card.entries[0]?.kind === "block" &&
      card.entries[0].block.inputCommandId === run
    ) {
      card.entries.push(entry);
      continue;
    }
    // Карточку узнают по ходу: её первая строка меняется, когда приходит старшая страница.
    const base: string = `answer:${run || entry.block.id}`;
    card = {
      key: seenRuns.has(base) ? `${base}:${entry.key}` : base,
      kind: "answer",
      entries: [entry],
    };
    seenRuns.add(base);
    rows.push(card);
  }
  return rows;
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
