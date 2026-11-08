import { getRuntime } from '#lib/app/runtime.js';
import type { PlanModel, PlanTask } from '#lib/stores/plan.svelte.js';

/** Задачи планов владельца: живые, событие хаба правит их на месте. Вне приложения — `null`. */
export function livePlan(): PlanModel | null {
  try {
    return getRuntime().list.plan;
  } catch {
    return null;
  }
}

/** Задача сейчас: по id, а без него — по номеру в плане оркестратора. */
export function liveTask(plan: PlanModel | null, orchestratorId: string, task: { id?: string; number: number }): PlanTask | null {
  if (!plan) return null;
  if (task.id) return plan.tasks.get(task.id) ?? null;
  if (!orchestratorId) return null;
  for (const known of plan.tasks.values()) if (known.orchestrator_id === orchestratorId && known.number === task.number) return known;
  return null;
}

/** Адрес плана оркестратора. */
export function planHref(orchestratorId: string): string {
  return `/plan/${orchestratorId}`;
}
