import { harnessLabel, modelLabel } from "#lib/format/labels.js";
import { fmt } from "#lib/format/ru.js";
import { utf8Length } from "#lib/stores/blocks.js";
import type { BackgroundFact } from "#lib/stores/background.svelte.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import { subagentVerb } from "#lib/stores/delegation.js";
import type { InputUiState } from "#lib/stores/inputs.js";

export const TOOL_PAGE_BYTES = 256 * 1024;
export const TOOL_DOM_CAP = 2 * 1024 * 1024;
export const REVEAL_LAG_MS = 600;
const TOOL_MS_BELOW = 100;
const TOOL_PILL_FROM_MS = 1000;
/** У Claude Code между началом и финалом вызова только запись в журнал: дольше живут вызовы, которые считают время сами. */
const TOOL_OWN_TIME_MIN_MS = 100;
/** Так воркер помечает сводку результата, завершившегося ошибкой. */
const TOOL_FAILED_MARK = "error";
const CLI_ERROR_OPEN = "<tool_use_error>";
/** Так воркер помечает ответ CLI на запуск фоновой задачи: это ещё не результат. */
const TOOL_LAUNCHED_MARK = "async_launched";

export function isLaunchResult(result: BlockModel | null): boolean {
  // Сводка приходит с финалом блока и сама по себе не реактивна.
  return !!result && result.state !== "streaming" && result.summary === TOOL_LAUNCHED_MARK;
}

export function deliveryLabel(
  state: InputUiState,
  agentWorking = false,
): string {
  switch (state) {
    case "saving":
      return "Сохраняется";
    case "queued":
      return "В очереди";
    case "dispatched":
    case "on_machine":
      return "На машине";
    case "transmitting":
      return "Передаётся";
    case "acceptance_unconfirmed":
      return "Приём не подтверждён";
    case "accepted":
      return agentWorking ? "Агент работает" : "Принято агентом";
    case "rejected":
      return "Отклонено";
    case "cancelled":
      return "Отменено";
  }
}

export function deliveryIndex(state: InputUiState): number {
  switch (state) {
    case "saving":
    case "queued":
      return 0;
    case "dispatched":
    case "on_machine":
      return 1;
    case "transmitting":
      return 2;
    case "acceptance_unconfirmed":
    case "rejected":
      return 3;
    case "accepted":
    case "cancelled":
      return 4;
  }
}

export function liveLineText(
  phase: string,
  elapsedMs: number,
  tool = "",
): string {
  const whole = Math.max(0, Math.floor(elapsedMs / 1000) * 1000);
  const label =
    phase === "error"
      ? "Ошибка в ответе"
      : phase === "done"
        ? "Ответ завершён"
        : phase === "tool"
          ? `Выполняет ${tool || "инструмент"}`
          : phase === "thinking"
            ? "Рассуждает"
            : "Генерирует";
  return `${label} · ${fmt.duration(whole)}`;
}

export function toolStatusText(opts: {
  running: boolean;
  error: boolean;
  startedAt: number;
  finishedAt: number | null;
  now: number;
}): string {
  if (opts.error) return "Ошибка";
  // Завершённый инструмент без отметки конца считал бы время вечно.
  if (!opts.running && opts.finishedAt === null) return "";
  const end = opts.running ? opts.now : (opts.finishedAt as number);
  const elapsed = Math.max(0, end - opts.startedAt);
  if (!opts.running && elapsed < TOOL_MS_BELOW)
    return `${Math.max(1, Math.round(elapsed))} мс`;
  const shown = opts.running ? Math.floor(elapsed / 1000) * 1000 : elapsed;
  return fmt.duration(shown);
}

/** В строке время важно, пока идёт работа или если её было заметно: сотни миллисекунд ни о чём не говорят. */
export function toolPillText(opts: Parameters<typeof toolStatusText>[0]): string {
  const quick =
    !opts.error &&
    !opts.running &&
    opts.finishedAt !== null &&
    opts.finishedAt - opts.startedAt < TOOL_PILL_FROM_MS;
  return quick ? "" : toolStatusText(opts);
}

/** Начало и конец работы инструмента. Вызов у Claude Code финален сразу, время даёт результат. */
export function toolTiming(
  call: BlockModel,
  result: BlockModel | null,
  working: boolean,
  background?: BackgroundFact | null,
): { running: boolean; startedAt: number; finishedAt: number | null } {
  const startedAt = call.startedAt ?? call.createdAt;
  if (background) {
    const running = background.state === "running";
    return { running, startedAt, finishedAt: running ? null : background.endedAt };
  }
  if (isLaunchResult(result)) return toolTiming(call, null, working);
  // Такой вызов воркер придержал до результата: начало в ленте не совпадает с настоящим.
  if (call.approvedBy === "cli_policy" || call.kind === "tool_result")
    return { running: false, startedAt, finishedAt: null };
  if (result) {
    const done = result.state === "final" || result.state === "incomplete";
    return {
      running: !done,
      startedAt,
      finishedAt: done ? (result.finishedAt ?? result.createdAt) : null,
    };
  }
  const own = (call.finishedAt ?? startedAt) - startedAt;
  if (call.state !== "streaming" && own >= TOOL_OWN_TIME_MIN_MS)
    return { running: false, startedAt, finishedAt: call.finishedAt };
  return {
    running: call.state === "streaming" || working,
    startedAt,
    finishedAt: null,
  };
}

export function toolFailed(result: BlockModel | null): boolean {
  if (!result) return false;
  return result.summary === TOOL_FAILED_MARK || result.text.startsWith(CLI_ERROR_OPEN);
}

/** Вызов считается неудачным и без своего результата: обрыв хода и старые строки без вызова. */
export function toolError(block: BlockModel, result: BlockModel | null): boolean {
  return (
    block.state === "incomplete" ||
    block.kind === "error" ||
    toolFailed(result ?? (block.kind === "tool_result" ? block : null))
  );
}

/** Обёртку CLI вокруг текста ошибки владельцу показывать незачем. */
export function cleanToolOutput(text: string): string {
  if (/^\(\w+ completed with no output\)\s*$/.test(text)) return "";
  const inner = /^<tool_use_error>([\s\S]*?)<\/tool_use_error>\s*$/.exec(text);
  return inner ? (inner[1] ?? "") : text;
}

const MAIN_AGENTS = new Set(["orchestrator", "main", ""]);

/** `orchestrator` — внутренняя метка хаба и адаптеров, владелец видит название инструмента сессии. */
export function isMainAgent(agent: string): boolean {
  return MAIN_AGENTS.has(agent);
}

export function agentTitle(
  agent: string,
  harness: { title?: string; provider?: string } | null | undefined,
): string {
  if (isMainAgent(agent)) return harnessLabel(harness);
  return agent.charAt(0).toLocaleUpperCase("ru-RU") + agent.slice(1);
}

export const ASK_TOOL = "AskUserQuestion";
const TOOL_NAMES: Record<string, string> = { [ASK_TOOL]: "Вопрос" };

export interface ToolTitle {
  name: string;
  server: string;
  argument: string;
}

function mcpServer(raw: string): string {
  const plugin = /^plugin_(.+)_\1$/.exec(raw);
  return plugin?.[1] ?? raw.replace(/^plugin_/, "");
}

/** Сводка воркера начинается с имени инструмента, а имя MCP несёт служебный префикс: в строке всё это по одному разу. */
export function toolTitle(block: Pick<BlockModel, "toolName" | "summary">): ToolTitle {
  const raw = block.toolName ?? "";
  const mcp = /^mcp__(.+?)__(.+)$/.exec(raw);
  const name = mcp?.[2] ?? TOOL_NAMES[raw] ?? raw;
  const server = mcp ? mcpServer(mcp[1] ?? "") : "";
  let argument = block.summary ?? "";
  if (raw && argument === raw) argument = "";
  for (const joint of [": ", ", "])
    if (raw && argument.startsWith(raw + joint))
      argument = argument.slice(raw.length + joint.length);
  return { name, server: name.includes(server) ? "" : server, argument };
}

/** Отчёт субагента без служебной рамки CLI: она адресована модели, а не владельцу. */
export function handBackText(text: string): string {
  const frame = /^\[Subagent hand-back\][\s\S]*?The report follows:\n/.exec(text);
  return frame ? text.slice(frame[0].length).replace(/^ {2}/gm, "") : text;
}

export function firstLine(text: string): string {
  return cleanToolOutput(text).split("\n").find((line) => line.trim().length > 0)?.trim() ?? "";
}

const FILE_TOOLS = new Set(["Read", "Edit", "MultiEdit", "Write", "NotebookEdit"]);
const COMMAND_TOOL = /bash|shell|exec|command/i;

/** Строка вызова: у файла сначала имя, у команды только первая строка, и в старых сводках тоже. */
export function toolLabel(title: ToolTitle): { text: string; dir: string } {
  if (FILE_TOOLS.has(title.name)) {
    const slash = title.argument.lastIndexOf("/");
    if (slash > 0)
      return { text: title.argument.slice(slash + 1), dir: title.argument.slice(0, slash) };
  }
  const first = title.argument.split("\n")[0] ?? "";
  return { text: (first.split("<<")[0] ?? "").trimEnd(), dir: "" };
}

export function callCaption(block: Pick<BlockModel, "toolName">): string {
  return COMMAND_TOOL.test(block.toolName ?? "") ? "Команда" : "Вызов";
}

export function takeToolPage(
  shown: string,
  page: string,
): { text: string; capped: boolean; saveAsFile: boolean } {
  if (utf8Length(shown) + utf8Length(page) > TOOL_DOM_CAP)
    return { text: shown, capped: true, saveAsFile: true };
  return { text: shown + page, capped: false, saveAsFile: false };
}

export function revealedText(
  full: string,
  elapsedMs: number,
  motionScale: number,
): string {
  if (motionScale === 0 || elapsedMs >= REVEAL_LAG_MS) return full;
  const parts = full.split(/(\s+)/);
  const words: number[] = [];
  parts.forEach((part, index) => {
    if (part.trim()) words.push(index);
  });
  if (words.length === 0) return full;
  const count = Math.max(
    1,
    Math.ceil(
      (words.length * Math.min(REVEAL_LAG_MS, Math.max(0, elapsedMs))) /
        REVEAL_LAG_MS,
    ),
  );
  const last = words[Math.min(words.length, count) - 1];
  if (last === undefined) return "";
  return parts.slice(0, last + 1).join("");
}

const SUBAGENT_END = { done: "Готово", error: "Ошибка", stopped: "Прервано" };

export type SubagentState = "running" | "done" | "error" | "stopped";

const BACKGROUND_STATE: Record<string, SubagentState> = {
  running: "running",
  completed: "done",
  failed: "error",
  stopped: "stopped",
};

export function subagentCopy(
  call: BlockModel,
  children: BlockModel[],
  end: {
    result: BlockModel | null;
    working: boolean;
    background?: BackgroundFact | null;
  },
): { title: string; detail: string; state: SubagentState } {
  const own = children.filter((child) => !isMainAgent(child.agent));
  const lead = own[0];
  const action = own.findLast((child) => child.toolName) ?? null;
  const model = lead?.model || call.model;
  const running = toolTiming(call, end.result, end.working, end.background).running;
  const result = isLaunchResult(end.result) ? null : end.result;
  const state: SubagentState = end.background
    ? BACKGROUND_STATE[end.background.state] ?? "stopped"
    : toolFailed(result)
      ? "error"
      : running
        ? "running"
        : result
          ? "done"
          : "stopped";
  const doing = action ? toolTitle(action) : null;
  const object = doing?.argument || doing?.name || "";
  const task = toolTitle(call).argument;
  const verb = subagentVerb(action?.toolName ?? (lead || end.background ? null : call.toolName));
  const line =
    state === "running"
      ? [verb, object].filter((part) => part.length > 0).join(" ")
      : SUBAGENT_END[state];
  return {
    title: ["Субагент", lead?.agent ?? "", model ? modelLabel(model) : ""]
      .filter((part) => part.length > 0)
      .join(" · "),
    detail: task ? `${line} · ${task}` : line,
    state,
  };
}

export function scheduledLabel(sendAt: number): string {
  return `Отправится в ${fmt.clock(sendAt)}`;
}
