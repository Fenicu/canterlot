import type { ListRow } from "#lib/stores/list.svelte.js";

/** Срок «Отложить» ещё впереди; `null` — сессия не отложена. */
export function snoozedUntil(item: ListRow, now = Date.now()): number | null {
  const at = item.snoozed_until ? Date.parse(item.snoozed_until) : Number.NaN;
  return Number.isFinite(at) && at > now ? at : null;
}

export function isSnoozed(item: ListRow, now = Date.now()): boolean {
  return snoozedUntil(item, now) !== null;
}

/** Время сброса лимита из паузы или удержания сессии; `null` — лимита с известным сроком нет. */
export function limitResetAt(queue?: {
  pause_reasons?: unknown[];
  hold_reasons?: unknown[];
} | null): number | null {
  if (!queue) return null;
  const ms = (value: unknown): number | null => {
    if (typeof value !== "string") return null;
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : null;
  };
  for (const raw of queue.pause_reasons ?? []) {
    const reason = raw as { reason?: string; resets_at?: string | null };
    if (reason.reason !== "limit_reached") continue;
    const at = ms(reason.resets_at);
    if (at !== null) return at;
  }
  for (const raw of queue.hold_reasons ?? []) {
    const hold = raw as { reason?: string; wake_at?: string | null };
    if (hold.reason !== "quota_exhausted") continue;
    const at = ms(hold.wake_at);
    if (at !== null) return at;
  }
  return null;
}

export type SnoozeChoice =
  | { kind: "hour" }
  | { kind: "three_hours" }
  | { kind: "morning" }
  | { kind: "limit"; at: number }
  | { kind: "custom" };

/** «До утра» — следующий 09:00 по местному времени устройства. */
export function nextMorning(now = new Date()): Date {
  const morning = new Date(now);
  morning.setHours(9, 0, 0, 0);
  if (morning.getTime() <= now.getTime()) morning.setDate(morning.getDate() + 1);
  return morning;
}

export function snoozeChoices(now: number, resetAt: number | null): SnoozeChoice[] {
  const choices: SnoozeChoice[] = [
    { kind: "hour" },
    { kind: "three_hours" },
    { kind: "morning" },
  ];
  if (resetAt !== null && resetAt > now) choices.push({ kind: "limit", at: resetAt });
  choices.push({ kind: "custom" });
  return choices;
}

export function choiceLabel(choice: SnoozeChoice): string {
  switch (choice.kind) {
    case "hour":
      return "1 час";
    case "three_hours":
      return "3 часа";
    case "morning":
      return "До утра";
    case "limit":
      return "До сброса лимита";
    case "custom":
      return "Выбрать время…";
  }
}

/** Абсолютное время выбора; для «Выбрать время…» его даёт поле. */
export function choiceUntil(choice: SnoozeChoice, now = new Date()): Date | null {
  switch (choice.kind) {
    case "hour":
      return new Date(now.getTime() + 60 * 60_000);
    case "three_hours":
      return new Date(now.getTime() + 3 * 60 * 60_000);
    case "morning":
      return nextMorning(now);
    case "limit":
      return new Date(choice.at);
    case "custom":
      return null;
  }
}
