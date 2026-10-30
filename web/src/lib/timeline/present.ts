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
      return "Принято";
    case "accepted":
      return agentWorking ? "агент работает" : "доставлено";
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

export function liveLineText(phase: string, elapsedMs: number): string {
  const whole = Math.max(0, Math.floor(elapsedMs / 1000) * 1000);
  const label =
    phase === "error"
      ? "Ошибка в ответе"
      : phase === "done"
        ? "Ответ завершён"
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
  const first = children[0];
  const name = first?.agent || call.agent || "агент";
  const model = first?.model || call.model || "";
  const verb = subagentVerb(first?.toolName ?? call.toolName);
  const object = first?.summary || first?.toolName || "";
  const task = call.summary ?? "";
  const detail = [verb, object].filter((part) => part.length > 0).join(" ");
  return {
    title: `Субагент · ${name} · ${model}`.replace(/\s·\s$/, ""),
    detail: task ? `${detail} · ${task}` : detail,
  };
}

export function scheduledLabel(sendAt: number): string {
  return `Отправится в ${fmt.clock(sendAt)}`;
}
