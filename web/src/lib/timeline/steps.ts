import { fmt } from "#lib/format/ru.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import type { FeedFolds } from "./folds";
import { toolError, toolTiming, toolTitle } from "./present";
import { isSubagentCall, type BlockLinks } from "./rows";

const MIN_STEPS = 3;

export type BlockEntry = Extract<TimelineEntry, { kind: "block" }>;

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

/** Подряд идущие служебные вызовы одной бледной строкой. */
export interface QuietGroup {
  kind: "quiet";
  key: string;
  entries: TimelineEntry[];
  /** Чьи вызовы: `meta` — служебные, `explore` — чтения и поиски по коду. */
  family: string;
  /** Следом идёт ещё один вызов той же работы, и он не закончен. */
  running: boolean;
  /** Вызовов с ошибкой внутри. */
  failed: number;
}

/** Семейство чтений и поисков: свёрнутая строка «Изучает код». */
export const EXPLORE = "explore";

export type CardItem = StepGroup | PlainItem | QuietGroup;

export interface FoldContext {
  links: BlockLinks;
  /** Ход этой карточки идёт. */
  working: boolean;
  live: boolean;
  folds: FeedFolds;
  /** Строка, которая остаётся на виду, даже если похожа на шаг. */
  stays?: (block: BlockModel) => boolean;
  /** Служебный вызов: подряд идущие собираются в одну бледную строку. */
  quiet?: (block: BlockModel) => boolean;
  /** Имя шага для подсчёта в свёрнутой группе: у карточек оно своё. */
  label?: (block: BlockModel) => string | null;
  /** Семейство сворачивания карточки: `explore` у чтений и поисков. */
  fold?: (block: BlockModel) => string | null;
}

function explores(entry: TimelineEntry, ctx: FoldContext): entry is BlockEntry {
  return entry.kind === "block" && entry.block.kind === "tool_call" && ctx.fold?.(entry.block) === EXPLORE;
}

function isTool(entry: TimelineEntry): entry is BlockEntry {
  return entry.kind === "block" && (entry.block.kind === "tool_call" || entry.block.kind === "tool_result");
}

function isStep(entry: TimelineEntry, ctx: FoldContext): entry is BlockEntry {
  if (entry.kind !== "block" || ctx.stays?.(entry.block)) return false;
  // Служебные вызовы собираются своей бледной строкой, а не шагами хода. Чтения и поиски
  // тоже бледные, но остаются шагами: иначе они рвали бы группы «N шагов» на куски.
  if (entry.block.kind === "tool_call" && ctx.quiet?.(entry.block) && !explores(entry, ctx)) return false;
  return entry.block.kind === "reasoning" || isTool(entry);
}

function running(block: BlockModel, ctx: FoldContext): boolean {
  if (block.state === "streaming" || block.state === "stale") return true;
  if (block.kind === "reasoning") return false;
  return toolTiming(block, ctx.links.result(block.id), ctx.working, ctx.links.background(block.id)).running;
}

/** Имя шага в счётчике и в живом окне: одно и то же у группы и у окна. */
export function stepName(block: BlockModel, ctx: Pick<FoldContext, "label">): string {
  if (isSubagentCall(block)) return "Субагент";
  return ctx.label?.(block) || toolTitle(block).name || "Шаг";
}

/** Счётчик вызовов хода: «Правка 7 · Команда 5». Общий у свёрнутой группы и шапки живого окна. */
export function tallyOf(tools: BlockEntry[], ctx: Pick<FoldContext, "label">): string {
  const counts = new Map<string, number>();
  for (const { block } of tools) counts.set(stepName(block, ctx), (counts.get(stepName(block, ctx)) ?? 0) + 1);
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
    tally: tallyOf(tools, ctx),
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
  let quiet: TimelineEntry[] = [];
  const plain = (entry: TimelineEntry): void => {
    items.push({ kind: "entry", key: entry.key, entry });
  };
  const quietCall = (entry: TimelineEntry): boolean =>
    entry.kind === "block" && entry.block.kind === "tool_call" && ctx.quiet?.(entry.block) === true && !explores(entry, ctx);
  const closeQuiet = (): void => {
    if (quiet.length >= 2) items.push({ kind: "quiet", key: `quiet:${quiet[0]!.key}`, entries: quiet, family: "meta", running: false, failed: 0 });
    else quiet.forEach(plain);
    quiet = [];
  };
  /**
   * Шаги, оставшиеся на виду, идут по одному, но подряд идущие чтения и поиски одного агента
   * сворачиваются в «Изучает код». Идущий вызов в свёртку не попадает: вызов, ждущий
   * разрешения владельца, всегда стоит своей строкой; свёртка перед ним говорит «идёт».
   */
  const spill = (entries: TimelineEntry[], nextExplores = false): void => {
    let reads: BlockEntry[] = [];
    const close = (last: boolean): void => {
      if (reads.length >= 2) {
        const failed = reads.filter((entry) => toolError(entry.block, ctx.links.result(entry.block.id))).length;
        items.push({ kind: "quiet", key: `quiet:${reads[0]!.key}`, entries: reads, family: EXPLORE, running: last && nextExplores, failed });
      } else reads.forEach(plain);
      reads = [];
    };
    for (const entry of entries) {
      if (explores(entry, ctx) && (reads.length === 0 || reads[0]!.block.agent === entry.block.agent)) {
        reads.push(entry);
        continue;
      }
      close(false);
      if (explores(entry, ctx)) reads.push(entry);
      else plain(entry);
    }
    close(true);
  };
  const flush = (following: number, nextExplores = false): void => {
    // Ход, где агент только читал и искал, — одна строка «Изучает код», а не «N шагов».
    const onlyExplores = run.length >= 2 && run.every((entry) => explores(entry, ctx) && entry.block.agent === (run[0] as BlockEntry).block.agent);
    if (!onlyExplores && run.filter(isTool).length + following >= MIN_STEPS) items.push(group(run, ctx, taken));
    else spill(run, nextExplores);
    run = [];
  };
  for (const entry of list) {
    if (quietCall(entry)) {
      if (run.length > 0) flush(0);
      quiet.push(entry);
      continue;
    }
    closeQuiet();
    if (isStep(entry, ctx) && !running(entry.block, ctx)) {
      run.push(entry);
      continue;
    }
    flush(isStep(entry, ctx) ? 1 : 0, explores(entry, ctx));
    plain(entry);
  }
  closeQuiet();
  const lastTool = ctx.live ? run.findLastIndex(isTool) : -1;
  const tail = lastTool >= 0 ? run.splice(lastTool) : [];
  flush(tail.length > 0 ? 1 : 0);
  spill(tail);
  return items;
}
