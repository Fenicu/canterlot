import { fmt } from "#lib/format/ru.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import type { FeedFolds } from "./folds";
import { toolError, toolTiming, toolTitle } from "./present";
import { isSubagentCall, type BlockLinks } from "./rows";

const MIN_STEPS = 3;

type BlockEntry = Extract<TimelineEntry, { kind: "block" }>;

export interface StepGroup {
  kind: "group";
  key: string;
  entries: TimelineEntry[];
  label: string;
  tally: string;
  failed: number;
  elapsed: number;
}

export interface PlainItem {
  kind: "entry";
  key: string;
  entry: TimelineEntry;
}

export type CardItem = StepGroup | PlainItem;

export interface FoldContext {
  links: BlockLinks;
  /** Ход этой карточки идёт. */
  working: boolean;
  live: boolean;
  folds: FeedFolds;
  /** Строка, которая остаётся на виду, даже если похожа на шаг. */
  stays?: (block: BlockModel) => boolean;
}

function isTool(entry: TimelineEntry): entry is BlockEntry {
  return entry.kind === "block" && (entry.block.kind === "tool_call" || entry.block.kind === "tool_result");
}

function isStep(entry: TimelineEntry, ctx: FoldContext): entry is BlockEntry {
  if (entry.kind !== "block" || ctx.stays?.(entry.block)) return false;
  return entry.block.kind === "reasoning" || isTool(entry);
}

function running(block: BlockModel, ctx: FoldContext): boolean {
  if (block.state === "streaming" || block.state === "stale") return true;
  if (block.kind === "reasoning") return false;
  return toolTiming(block, ctx.links.result(block.id), ctx.working, ctx.links.background(block.id)).running;
}

function stepName(block: BlockModel): string {
  if (isSubagentCall(block)) return "Субагент";
  return toolTitle(block).name || "Шаг";
}

function tallyOf(tools: BlockEntry[]): string {
  const counts = new Map<string, number>();
  for (const { block } of tools) counts.set(stepName(block), (counts.get(stepName(block)) ?? 0) + 1);
  return [...counts]
    .sort((a, b) => b[1] - a[1])
    .map(([name, count]) => `${name} ${count}`)
    .join(" · ");
}

function group(entries: TimelineEntry[], ctx: FoldContext, taken: Set<string>): StepGroup {
  const tools = entries.filter(isTool);
  let start = Number.POSITIVE_INFINITY;
  let end = 0;
  let failed = 0;
  for (const entry of entries) {
    if (entry.kind !== "block") continue;
    const { block } = entry;
    const result = block.kind === "reasoning" ? null : ctx.links.result(block.id);
    start = Math.min(start, block.startedAt ?? block.createdAt);
    end = Math.max(end, result?.finishedAt ?? result?.createdAt ?? block.finishedAt ?? block.createdAt);
    if (block.kind !== "reasoning" && toolError(block, result)) failed += 1;
  }
  return {
    kind: "group",
    key: ctx.folds.groupKey(entries.map((entry) => entry.key), taken),
    entries,
    label: `${tools.length} ${fmt.plural(tools.length, ["шаг", "шага", "шагов"])}`,
    tally: tallyOf(tools),
    failed,
    elapsed: Math.max(0, end - start),
  };
}

/**
 * Сворачивает подряд идущие шаги хода в одну строку. Текст, запросы и ошибки остаются на виду.
 * Идущий шаг в группу не попадает, а пока ход идёт, последний шаг стоит отдельно: следующий
 * занимает его место, и высота карточки не меняется, когда шаги уходят в группу.
 */
export function foldSteps(list: TimelineEntry[], ctx: FoldContext): CardItem[] {
  const items: CardItem[] = [];
  const taken = new Set<string>();
  let run: TimelineEntry[] = [];
  const plain = (entry: TimelineEntry): void => {
    items.push({ kind: "entry", key: entry.key, entry });
  };
  const flush = (following: number): void => {
    if (run.filter(isTool).length + following >= MIN_STEPS) items.push(group(run, ctx, taken));
    else run.forEach(plain);
    run = [];
  };
  for (const entry of list) {
    if (isStep(entry, ctx) && !running(entry.block, ctx)) {
      run.push(entry);
      continue;
    }
    flush(isStep(entry, ctx) ? 1 : 0);
    plain(entry);
  }
  const lastTool = ctx.live ? run.findLastIndex(isTool) : -1;
  const tail = lastTool >= 0 ? run.splice(lastTool) : [];
  flush(tail.length > 0 ? 1 : 0);
  tail.forEach(plain);
  return items;
}
