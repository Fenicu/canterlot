import { modelLabel } from "#lib/format/labels.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import { isMainAgent, toolTitle } from "#lib/timeline/present.js";

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
  opts?: { machineName?: string },
): LaneModelResult {
  const root = blocks.find((b) => b.id === rootBlockId);
  const lead = blocks.find((block) => block.parentId === rootBlockId && !isMainAgent(block.agent));
  const agentName = lead?.agent ?? (root && !isMainAgent(root.agent) ? root.agent : "Субагент");
  const model = lead?.model || root?.model || "";
  const machine = opts?.machineName || "workstation";
  const task = (root && toolTitle(root).argument) || null;

  const title = task || agentName;
  const subtitle = ["Субагент", agentName === "Субагент" ? "" : agentName, model ? modelLabel(model) : "", machine]
    .filter(Boolean)
    .join(" · ");
  const breadcrumb = ["Оркестратор", agentName];

  let status = "Готово";
  let running = false;

  if (root) {
    const isStreaming =
      root.state === "streaming" ||
      blocks.some((b) => b.parentId === rootBlockId && b.state === "streaming");

    if (isStreaming) {
      status = "Работает параллельно с основным агентом";
      running = true;
    } else if (root.state === "incomplete" || root.state === "stale") {
      status = "Прервано";
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
