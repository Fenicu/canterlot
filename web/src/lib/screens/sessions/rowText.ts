import type { ListRow } from '#lib/stores/list.svelte.js';
import type { MachineRecord } from '#lib/stores/machines.svelte.js';
import { modelShortName } from '#lib/vendors/vendor.js';
import { activityLine, type Activity, type Tone } from './activityText';

export interface RowLine {
  /** Короткое имя модели, с машиной, когда владельцу доступны несколько машин. */
  who: string;
  text: string;
  tone: Tone;
  live: boolean;
  /** Строка показывает превью ответа, а не состояние. */
  preview: boolean;
}

/** Название модели из каталога машины, на которой идёт сессия. */
export function catalogTitle(item: ListRow, machine: MachineRecord | null | undefined): string | null {
  const harness = machine?.harnesses.find((entry) => entry.id === item.harness.id);
  return harness?.models.find((model) => model.id === item.model)?.title ?? null;
}

/** Сколько машин владельца на связи: при двух и больше строка называет машину. */
export function onlineMachines(machines: Iterable<MachineRecord>): number {
  let count = 0;
  for (const machine of machines) if (machine.availability === 'available' || machine.availability === 'ready' || machine.availability === 'degraded') count += 1;
  return count;
}

/**
 * Вторая строка сессии: «модель: текст». Пока что-то идёт или ждёт владельца — состояние
 * в его цвете; в покое — начало последнего ответа агента, а без него — прежнее состояние.
 */
export function rowLine(
  item: ListRow,
  now: number,
  context: { title?: string | null; machines?: number } = {}
): RowLine {
  const line = activityLine(item.activity as Activity, { now });
  const model = modelShortName(item.model, item.model_resolved, context.title);
  const who = (context.machines ?? 1) > 1 ? `${model} · ${item.machine.name}` : model;
  const calm = item.activity.state === 'idle' && line.tone === 'default';
  const preview = item.preview?.trim() ?? '';
  if (calm && preview) return { who, text: preview, tone: 'default', live: false, preview: true };
  return { who, text: line.text, tone: line.tone, live: line.live, preview: false };
}
