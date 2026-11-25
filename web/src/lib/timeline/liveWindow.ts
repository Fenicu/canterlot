import { stripAnsi } from "#lib/render/ansi.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import type { TimelineEntry } from "#lib/stores/timeline.js";
import { firstLine, toolError, toolLabel, toolTiming, toolTitle } from "./present";
import type { BlockLinks } from "./rows";
import { stepName, tallyOf, type BlockEntry } from "./steps";
import { apiErrorView } from "./system";

/** Сколько последних строк окна рисуется: всё, что выше, всё равно срезано верхом. */
export const WINDOW_LIMIT = 12;

/** Шапка окна: всё, что владельцу нужно знать о ходе целиком, а не о видимом хвосте. */
export interface WindowHead {
  /** Вызовов инструментов в ходе. */
  count: number;
  /** Тот же счётчик, что у свёрнутой группы: «Правка 7 · Команда 5». */
  tally: string;
  /** Вызовов с ошибкой. */
  failed: number;
  /** Начало первого шага хода: от него идут часы в шапке. */
  startedAt: number;
}

export type WindowItem =
  | { kind: "reasoning"; key: string; block: BlockModel }
  | { kind: "text"; key: string; block: BlockModel }
  | { kind: "notice"; key: string; block: BlockModel }
  | {
      kind: "tool";
      key: string;
      block: BlockModel;
      /** Имя шага для счётчика: у субагента «Субагент», у карточки — её короткое имя. */
      name: string;
      /** Строка вызова, как у `ToolBlock`: имя сервера и аргумент либо начало текста. */
      detail: string;
      result: BlockModel | null;
      failed: boolean;
      running: boolean;
      startedAt: number;
      finishedAt: number | null;
    }
  /** Результат, чей вызов остался в другой карточке: одна строка начала вывода. */
  | { kind: "result"; key: string; block: BlockModel };

export interface LiveWindowView {
  /** Записи до первого шага: вступление, как сегодня. */
  before: TimelineEntry[];
  /** Сжатые строки хода в порядке ленты, хвостом не длиннее `WINDOW_LIMIT`. */
  stream: WindowItem[];
  /** Запросы: им нужно действие владельца, они стоят под окном. */
  requests: TimelineEntry[];
  head: WindowHead;
}

export interface LiveWindowContext {
  links: BlockLinks;
  /** Ход идёт: вызов без результата ещё работает. */
  working: boolean;
  /** Строка, которая остаётся на виду в свёрнутой ленте, шагом не считается. */
  stays?: (block: BlockModel) => boolean;
  /** Короткое имя шага для счётчика. */
  label?: (block: BlockModel) => string | null;
  harness?: { id?: string; provider?: string } | null;
}

function isRequest(entry: TimelineEntry): boolean {
  return entry.kind === "request" || entry.kind === "secret_request";
}

/**
 * Шаг живого окна: рассуждение, вызов инструмента или его результат. Служебные вызовы,
 * чтения и субагенты тоже шаги: бледной строки в окне нет. Строка, которая в ленте
 * держится на виду (карточки), шагом не считается — она открывает окно, только если
 * раньше не было шагов, и остаётся во вступлении.
 */
function isStep(entry: TimelineEntry, ctx: LiveWindowContext): entry is BlockEntry {
  if (entry.kind !== "block" || ctx.stays?.(entry.block)) return false;
  const kind = entry.block.kind;
  return kind === "reasoning" || kind === "tool_call" || kind === "tool_result";
}

function toolItem(entry: BlockEntry, ctx: LiveWindowContext): WindowItem {
  const { block } = entry;
  const result = ctx.links.result(block.id);
  const timing = toolTiming(block, result, ctx.working, ctx.links.background(block.id));
  const title = toolTitle(block);
  const label = toolLabel(title);
  return {
    kind: "tool",
    key: entry.key,
    block,
    name: stepName(block, ctx),
    detail: title.name ? [title.server, label.text].filter(Boolean).join(" · ") : firstLine(stripAnsi(block.text)),
    result,
    failed: toolError(block, result),
    running: timing.running || block.state === "stale",
    startedAt: timing.startedAt,
    finishedAt: timing.finishedAt
  };
}

/**
 * Разбирает записи живого хода на вступление, окно и запросы. Окно показывает ход
 * целиком, но рисует только последние строки: шапка считает все вызовы, а не видимые.
 * Результат, чей вызов есть в карточке, отдельной строкой не встаёт — он вывод вызова.
 */
export function liveWindow(entries: TimelineEntry[], ctx: LiveWindowContext): LiveWindowView {
  const first = entries.findIndex((entry) => isStep(entry, ctx));
  const prefix = first < 0 ? entries : entries.slice(0, first);
  const before = prefix.filter((entry) => !isRequest(entry));
  const requests = entries.filter(isRequest);
  const calls = new Set<string>();
  for (const entry of entries)
    if (entry.kind === "block" && entry.block.kind === "tool_call") calls.add(entry.block.id);

  const items: WindowItem[] = [];
  const tools: BlockEntry[] = [];
  let startedAt = 0;
  let start = Number.POSITIVE_INFINITY;
  for (const entry of first < 0 ? [] : entries.slice(first)) {
    if (isRequest(entry) || entry.kind !== "block") continue;
    if (isStep(entry, ctx)) start = Math.min(start, entry.block.startedAt ?? entry.block.createdAt);
    const { block } = entry;
    if (block.kind === "tool_result") {
      const call = ctx.links.parent(block) ?? block.parentId;
      if (call !== null && calls.has(call)) continue;
      items.push({ kind: "result", key: entry.key, block });
      tools.push(entry);
      continue;
    }
    if (block.kind === "tool_call") {
      items.push(toolItem(entry, ctx));
      tools.push(entry);
      continue;
    }
    if (block.kind === "reasoning") {
      items.push({ kind: "reasoning", key: entry.key, block });
      continue;
    }
    // Ошибку провайдера показывает свой пузырь ленты, а не строка окна.
    if ((block.kind === "warning" || block.kind === "error") && !apiErrorView(block, ctx.harness)) {
      items.push({ kind: "notice", key: entry.key, block });
      continue;
    }
    if (block.kind === "text") items.push({ kind: "text", key: entry.key, block });
  }
  if (Number.isFinite(start)) startedAt = start;

  const failed = tools.filter(({ block }) => toolError(block, block.kind === "tool_call" ? ctx.links.result(block.id) : null)).length;
  return {
    before,
    stream: items.slice(-WINDOW_LIMIT),
    requests,
    head: { count: tools.length, tally: tallyOf(tools, ctx), failed, startedAt }
  };
}
