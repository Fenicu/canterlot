import type { MachineRecord } from "#lib/stores/machines.svelte.js";
import { machineProblem } from "./newSessionModel";

/** Сессия списка в объёме, нужном прошлым настройкам: остальное форма не читает. */
export interface PrefillSession {
  machine: { id: string };
  harness: { id: string };
  workdir: string;
  model: string;
  effort: string;
  created_at: string;
  groups: string[];
  trashed_at?: string | null;
  /** Сессия запущена агентом: её настройки владелец не выбирал. */
  parent_id?: string | null;
}

export interface SessionPrefill {
  machineId: string;
  harness: string;
  workdir: string;
  modelId: string;
  effort: string;
}

/**
 * Прошлая сессия — самая свежая по времени создания из созданных владельцем; в группе —
 * самая свежая из неё.
 */
export function latestSession(
  sessions: Iterable<PrefillSession>,
  group = "",
): PrefillSession | null {
  let best: PrefillSession | null = null;
  let bestAt = -Infinity;
  for (const item of sessions) {
    if (item.trashed_at) continue;
    // Субагент получил машину, модель и папку от оркестратора: это не выбор владельца.
    if (item.parent_id) continue;
    if (group && !item.groups.includes(group)) continue;
    const at = Date.parse(item.created_at);
    const stamp = Number.isFinite(at) ? at : 0;
    if (!best || stamp >= bestAt) {
      best = item;
      bestAt = stamp;
    }
  }
  return best;
}

/**
 * Значения прошлой сессии, которые ещё есть в каталоге: не подошедшее поле остаётся
 * пустым для ручного выбора, остальные переносятся.
 */
export function prefillFromSession(
  session: PrefillSession | null,
  machines: Iterable<MachineRecord>,
): SessionPrefill | null {
  if (!session) return null;
  const workdir = session.workdir;
  const empty: SessionPrefill = {
    machineId: "",
    harness: "",
    workdir,
    modelId: "",
    effort: "",
  };
  const machine = [...machines].find((item) => item.id === session.machine.id);
  if (!machine || machineProblem(machine)) return empty;
  const harness = machine.harnesses.find(
    (item) => item.id === session.harness.id,
  );
  if (!harness) return { ...empty, machineId: machine.id };
  const model = harness.models.find((item) => item.id === session.model);
  if (!model) return { ...empty, machineId: machine.id, harness: harness.id };
  return {
    machineId: machine.id,
    harness: harness.id,
    workdir,
    modelId: model.id,
    effort: model.efforts.includes(session.effort) ? session.effort : "",
  };
}

/** Дом в подписи сокращается до `~`: полный путь на телефоне не помещается. */
export function shortenHomePath(path: string, home: string): string {
  if (!home) return path;
  if (path === home) return "~";
  return path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;
}
