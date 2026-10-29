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

export function buildRows(
  entries: TimelineEntry[],
  opts?: { laneRoot?: string; now?: number },
): Row[] {
  const source = opts?.laneRoot
    ? descendants(entries, opts.laneRoot)
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

function descendants(entries: TimelineEntry[], root: string): TimelineEntry[] {
  const children = new Map<string, string[]>();
  for (const entry of entries) {
    if (entry.kind !== "block" || !entry.block.parentId) continue;
    const list = children.get(entry.block.parentId) ?? [];
    list.push(entry.block.id);
    children.set(entry.block.parentId, list);
  }
  const keep = new Set<string>();
  const walk = (id: string): void => {
    for (const child of children.get(id) ?? []) {
      keep.add(child);
      walk(child);
    }
  };
  walk(root);
  return entries.filter(
    (entry) => entry.kind === "block" && keep.has(entry.block.id),
  );
}

export function childBlocks(
  entries: TimelineEntry[],
  parentId: string,
): BlockModel[] {
  const found: BlockModel[] = [];
  for (const entry of entries) {
    if (entry.kind === "block" && entry.block.parentId === parentId)
      found.push(entry.block);
  }
  return found;
}
