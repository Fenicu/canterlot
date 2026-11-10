import { fmt } from "#lib/format/ru.js";

/** Как владелец хочет видеть рассуждения: строкой, живым окном или целиком. */
export type ReasoningPref = "line" | "live" | "expanded";
/** Что сейчас на экране: строка с началом, живое окно хвоста или полный текст. */
export type ReasoningPresentation = "collapsed" | "live" | "expanded";
/** Явный выбор владельца для одного блока; `null` — блока не касались. */
export type ReasoningChoice = "open" | "closed" | null;

const PREVIEW_CHARS = 120;
const TAIL_CHARS = 2000;
const LIST_MARKER = /^(?:#{1,6}\s+|>\s?|[-*+]\s+|\d+\.\s*)+/;
const FENCE = /^(?:`{3,}|~{3,})/;
const RULE = /^(?:[-*_]\s*){3,}$/;

/**
 * Тап владельца сильнее настройки: раскрытый блок остаётся раскрытым, закрытый —
 * закрытым, чем бы ни кончился поток. Без выбора решает настройка устройства.
 */
export function reasoningPresentation(
  preference: ReasoningPref,
  streaming: boolean,
  choice: ReasoningChoice,
): ReasoningPresentation {
  if (choice === "open") return "expanded";
  if (choice === "closed") return "collapsed";
  if (preference === "expanded") return "expanded";
  if (preference === "live" && streaming) return "live";
  return "collapsed";
}

/** Строка рассуждения без служебных знаков Markdown: в строке ленты это мысль, а не разметка. */
function stripLine(line: string): string {
  let text = line.trim();
  if (FENCE.test(text) || RULE.test(text)) return "";
  text = text.replace(LIST_MARKER, "");
  text = text.replace(/^(\*\*|__|`+)/, "").replace(/(\*\*|__|`+)$/, "");
  return text.replace(/\s+/g, " ").trim();
}

function meaningfulLines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map(stripLine)
    .filter((line) => line.length > 0);
}

/**
 * Начало мысли для свёрнутой строки: первая непустая строка без разметки.
 * В живом потоке важнее последняя строка — по ней видно, о чём агент думает сейчас.
 */
export function reasoningPreview(
  text: string,
  mode: "first" | "live" = "first",
): string {
  const lines = meaningfulLines(text);
  const line = mode === "live" ? lines.at(-1) : lines[0];
  return line ? [...line].slice(0, PREVIEW_CHARS).join("") : "";
}

/** Хвост для живого окна: весь поток не нужен, окно всё равно показывает последние строки. */
export function reasoningTail(text: string): string {
  if (text.length <= TAIL_CHARS) return text;
  const tail = text.slice(text.length - TAIL_CHARS);
  const line = tail.indexOf("\n");
  return line < 0 ? tail : tail.slice(line + 1);
}

/** Длительность законченного блока; без отметки конца её не показывают. */
export function reasoningDuration(block: {
  startedAt: number | null;
  createdAt: number;
  finishedAt: number | null;
}): string {
  if (block.finishedAt === null) return "";
  const start = block.startedAt ?? block.createdAt;
  if (!start) return "";
  return fmt.duration(Math.max(0, block.finishedAt - start));
}

/**
 * Отдельная строка хода не повторяет «Рассуждает», пока о нём говорит само
 * рассуждение: два одинаковых часа на экране расходились бы на полкадра.
 * Строка остаётся на месте с прерыванием и уходит в скрытый текст для диктора.
 */
export function liveLineQuiet(phase: string, ownReasoning: boolean): boolean {
  return phase === "thinking" && ownReasoning;
}
