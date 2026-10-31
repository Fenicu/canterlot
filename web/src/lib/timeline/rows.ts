import type { BlockModel } from "#lib/stores/blocks.js";
import { fmt } from "#lib/format/ru.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
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
}

export function blockLinks(entries: TimelineEntry[]): BlockLinks {
  const children = new Map<string, BlockModel[]>();
  for (const entry of entries) {
    if (entry.kind !== "block") continue;
    const block = entry.block;
    if (!block.parentId) continue;
    const list = children.get(block.parentId);
    if (list) list.push(block);
    else children.set(block.parentId, [block]);
  }
  return {
    children: (id) => children.get(id) ?? [],
    result: (id) =>
      children.get(id)?.find((child) => child.kind === "tool_result") ?? null,
  };
}

export function buildRows(
  entries: TimelineEntry[],
  opts?: { laneRoot?: string; now?: number; links?: BlockLinks },
): Row[] {
  const links = opts?.links ?? blockLinks(entries);
  const source = opts?.laneRoot
    ? descendants(entries, opts.laneRoot, links)
    : entries.filter(
        (entry) => entry.kind === "input" || entry.block.parentId === null,
      );
  const rows: Row[] = [];
  let card: Row | null = null;
  let day = "";
  const seenDays = new Set<string>();
  const now = opts?.now ?? Date.now();
  for (const entry of source) {
    if (entry.kind === "input" && entry.input.placement !== "feed") continue;
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
    card = {
      key: `answer:${entry.block.id}`,
      kind: "answer",
      entries: [entry],
    };
    rows.push(card);
  }
  return rows;
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
