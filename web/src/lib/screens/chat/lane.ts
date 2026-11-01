import type { BackgroundFact } from "#lib/stores/background.svelte.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import { harnessLabel, modelLabel } from "#lib/format/labels.js";
import { isLaunchResult, isMainAgent, toolFailed, toolInterrupted, toolTitle } from "#lib/timeline/present.js";

export interface LaneModelResult {
  title: string;
  subtitle: string;
  task: string | null;
  status: string;
  running: boolean;
  breadcrumb: string[];
}

export function laneModel(
  blocks: BlockModel[],
  rootBlockId: string,
  opts?: {
    machineName?: string;
    children?: BlockModel[];
    harness?: { title?: string; provider?: string } | null;
    /** Результат вызова субагента; пока его нет и ход идёт, субагент работает. */
    result?: BlockModel | null;
    background?: BackgroundFact | null;
    working?: boolean;
    /** Ветку субагента ещё читают у хаба или её не удалось прочитать. */
    load?: "idle" | "loading" | "ready" | "failed";
  },
): LaneModelResult {
  const root = blocks.find((b) => b.id === rootBlockId);
  const children = opts?.children ?? blocks.filter((block) => block.parentId === rootBlockId);
  const lead = children.find((block) => !isMainAgent(block.agent));
  const agentName = lead?.agent ?? (root && !isMainAgent(root.agent) ? root.agent : "Субагент");
  const model = lead?.model || root?.model || "";
  const machine = opts?.machineName || "workstation";
  const task = (root && toolTitle(root).argument) || null;

  const title = task || agentName;
  const subtitle = ["Субагент", agentName === "Субагент" ? "" : agentName, model ? modelLabel(model) : "", machine]
    .filter(Boolean)
    .join(" · ");
  const breadcrumb = [harnessLabel(opts?.harness), agentName];

  // Корня нет: что с субагентом, неизвестно, и «Готово» тут было бы враньём.
  let status = opts?.load === "failed" ? "Не удалось загрузить" : root ? "Готово" : "Загружаю…";
  let running = false;
  const background = opts?.background ?? null;
  const result = isLaunchResult(opts?.result ?? null) ? null : (opts?.result ?? null);

  if (root) {
    const isStreaming =
      root.state === "streaming" ||
      blocks.some((b) => b.parentId === rootBlockId && b.state === "streaming") ||
      (background ? background.state === "running" : opts?.working === true && !result);

    if (isStreaming) {
      status = "Работает параллельно с основным агентом";
      running = true;
    } else if (root.state === "incomplete" || root.state === "stale" || background?.state === "stopped" || toolInterrupted(result)) {
      status = "Прервано";
      running = false;
    } else if (toolFailed(result) || background?.state === "failed") {
      status = "Ошибка";
      running = false;
    } else {
      status = "Готово";
      running = false;
    }
  }

  return {
    title,
    subtitle,
    task,
    status,
    running,
    breadcrumb,
  };
}
