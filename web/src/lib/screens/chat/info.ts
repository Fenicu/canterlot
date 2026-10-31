import { harnessLabel, modelLabel } from "#lib/format/labels.js";
import type { RunInfo } from "#lib/stores/run.js";
import { isMainAgent } from "#lib/timeline/present.js";

export interface InfoModel {
  parts: { title: string; sub: string; model: string }[];
  tokens: { label: string; main: number; sub: number | null }[];
  duration: string | null;
  context: { used: number; window: number } | null;
  limit: { text: string; note: string | null } | null;
  pending: boolean;
}

export function formatDuration(ms: number): string {
  const totalSecs = Math.round(ms / 1000);
  if (totalSecs < 60) {
    return `${totalSecs} с`;
  }
  const mins = Math.floor(totalSecs / 60);
  const secs = totalSecs % 60;
  if (mins < 60) {
    return secs > 0 ? `${mins} мин ${secs} с` : `${mins} мин`;
  }
  const hours = Math.floor(mins / 60);
  const remainingMins = mins % 60;
  return remainingMins > 0 ? `${hours} ч ${remainingMins} мин` : `${hours} ч`;
}

export function summarizeRun(
  info: RunInfo,
  harness?: { title?: string; provider?: string } | null,
): InfoModel {
  const pending = !info.finished_at || info.duration_ms == null;

  const duration =
    info.duration_ms != null ? formatDuration(info.duration_ms) : null;

  const context = info.context
    ? {
        used: info.context.tokens,
        window: info.context.window_tokens ?? 200000,
      }
    : null;

  let limit: { text: string; note: string | null } | null = null;
  if (info.limit) {
    const pct = (info.limit.weekly_delta_permille / 10).toLocaleString(
      "ru-RU",
      {
        minimumFractionDigits: 1,
        maximumFractionDigits: 1,
      },
    );
    const text = info.limit.weekly_delta_permille < 1 ? "меньше 0,1% недели" : `≈ ${pct}% недели`;
    const note =
      info.limit.exact === false
        ? "Оценка приблизительная"
        : "Оценка по изменению квоты";
    limit = { text, note };
  }

  const parts: { title: string; sub: string; model: string }[] = [];
  const seenParts = new Set<string>();
  for (const p of info.parts ?? []) {
    if (
      p.kind === "main" ||
      !p.agent ||
      p.agent === "main" ||
      p.agent === "Claude Code" ||
      p.agent === "Codex" ||
      p.agent === "orchestrator"
    ) {
      parts.push({
        title: "Основной ответ",
        sub: isMainAgent(p.agent ?? "") ? harnessLabel(harness) : p.agent,
        model: modelLabel(p.model),
      });
    } else {
      parts.push({
        title: p.task || p.agent,
        sub: `Субагент · ${p.agent}`,
        model: modelLabel(p.model),
      });
    }
    // Хаб отдаёт часть на каждый блок: одинаковые строки сворачиваются в одну, иначе ключи списка совпадают.
    const added = parts[parts.length - 1]!;
    const signature = `${added.title}\u0000${added.sub}\u0000${added.model}`;
    if (seenParts.has(signature)) parts.pop();
    else seenParts.add(signature);
  }

  let mainInput = 0;
  let mainOutput = 0;
  let mainCacheRead = 0;
  let mainCacheWrite = 0;

  let subInput = 0;
  let subOutput = 0;
  let subCacheRead = 0;
  let subCacheWrite = 0;
  let hasSub = false;

  const usageList = info.usage ?? [];
  for (const u of usageList) {
    const isMain =
      u.kind === "main" ||
      !u.agent ||
      u.agent === "main" ||
      u.agent === "Claude Code" ||
      u.agent === "Codex" ||
      u.agent === "orchestrator";

    if (isMain) {
      mainInput += u.input_tokens ?? 0;
      mainOutput += u.output_tokens ?? 0;
      mainCacheRead += u.cache_read_tokens ?? 0;
      mainCacheWrite += u.cache_write_tokens ?? 0;
    } else {
      hasSub = true;
      subInput += u.input_tokens ?? 0;
      subOutput += u.output_tokens ?? 0;
      subCacheRead += u.cache_read_tokens ?? 0;
      subCacheWrite += u.cache_write_tokens ?? 0;
    }
  }

  const tokens = [
    { label: "Вход", main: mainInput, sub: hasSub ? subInput : null },
    { label: "Выход", main: mainOutput, sub: hasSub ? subOutput : null },
    {
      label: "Прочитано из кэша",
      main: mainCacheRead,
      sub: hasSub ? subCacheRead : null,
    },
    {
      label: "Записано в кэш",
      main: mainCacheWrite,
      sub: hasSub ? subCacheWrite : null,
    },
  ];

  return {
    parts,
    tokens,
    ["duration"]: duration,
    context,
    limit,
    pending,
  };
}
