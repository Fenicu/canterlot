import { harnessLabel, modelLabel } from "#lib/format/labels.js";
import { fmt } from "#lib/format/ru.js";
import { utf8Length } from "#lib/stores/blocks.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import { subagentVerb } from "#lib/stores/delegation.js";
import type { InputUiState } from "#lib/stores/inputs.js";

export const TOOL_PAGE_BYTES = 256 * 1024;
export const TOOL_DOM_CAP = 2 * 1024 * 1024;
export const REVEAL_LAG_MS = 600;

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
  const shown = opts.running ? Math.floor(elapsed / 1000) * 1000 : elapsed;
  return fmt.duration(shown);
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
  const name = mcp?.[2] ?? raw;
  const server = mcp ? mcpServer(mcp[1] ?? "") : "";
  let argument = block.summary ?? "";
  if (raw && argument === raw) argument = "";
  for (const joint of [": ", ", "])
    if (raw && argument.startsWith(raw + joint))
      argument = argument.slice(raw.length + joint.length);
  return { name, server: name.includes(server) ? "" : server, argument };
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

export function subagentCopy(
  call: BlockModel,
  children: BlockModel[],
): { title: string; detail: string } {
  const own = children.filter((child) => !isMainAgent(child.agent));
  const lead = own[0];
  const action = own.findLast((child) => child.toolName) ?? null;
  const model = lead?.model || call.model;
  const verb = subagentVerb(action?.toolName ?? (lead ? null : call.toolName));
  const doing = action ? toolTitle(action) : null;
  const object = doing?.argument || doing?.name || "";
  const task = toolTitle(call).argument;
  const detail = [verb, object].filter((part) => part.length > 0).join(" ");
  return {
    title: ["Субагент", lead?.agent ?? "", model ? modelLabel(model) : ""]
      .filter((part) => part.length > 0)
      .join(" · "),
    detail: task ? `${detail} · ${task}` : detail,
  };
}

export function scheduledLabel(sendAt: number): string {
  return `Отправится в ${fmt.clock(sendAt)}`;
}
