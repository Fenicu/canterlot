// Живое состояние экрана расписаний: список и уведомление об изменениях обновлений хаба.
// Подписка на область `schedules` нужна только для того, чтобы перечитать список и историю:
// отдельного снимка у расписаний нет, поэтому курсор берём из GET /api/v1/events.
import { eventsRange, listSchedules } from "#lib/api/client.js";
import type { AppliedEvent, RangeResult, ScopeApplier } from "#lib/sync/scope.js";
import type { components } from "#lib/api/generated/openapi.js";

type Schedule = components["schemas"]["Schedule"];

type Subscription = { release(): void };
type Connection = {
  subscribe(scope: "schedules", applier: ScopeApplier): Subscription;
};

type RangeBody = {
  events?: RangeResult extends { events: infer E } ? E : never;
  head?: string;
  resync_required?: unknown;
};

function rangeOf(body: unknown, after: string): RangeResult {
  const value = body as RangeBody | null;
  if (!value || typeof value !== "object" || "resync_required" in value) return { resync: true };
  return { events: value.events ?? [], head: value.head ?? after };
}

export class SchedulesStore {
  items = $state<Schedule[]>([]);
  /** Растёт на каждое событие расписания: экраны истории перечитывают журнал по этому признаку. */
  version = $state(0);
  error = $state("");
  #sub: Subscription | null = null;

  async load(): Promise<void> {
    try {
      this.items = (await listSchedules()).data;
      this.error = "";
    } catch {
      this.error = "Не удалось загрузить расписания. Повторите при восстановлении связи";
    }
  }

  open(connection: Connection): void {
    if (this.#sub) return;
    this.#sub = connection.subscribe("schedules", this.applier);
  }

  close(): void {
    this.#sub?.release();
    this.#sub = null;
  }

  private applier: ScopeApplier = {
    loadSnapshot: async () => {
      const list = await listSchedules();
      const response = await eventsRange({ scope: "schedules", limit: 1 }).catch(() => null);
      const body = response?.data as RangeBody | undefined;
      const cursor = body && typeof body.head === "string" && !("resync_required" in body)
        ? body.head
        : "0";
      return { cursor, data: list.data };
    },
    applySnapshot: (data: unknown) => {
      this.items = (data ?? []) as Schedule[];
    },
    applyEvents: (batch: AppliedEvent[]) => {
      if (!batch.some(({ event }) =>
        event.kind === "schedule_changed" || event.kind === "schedule_run_changed")) return;
      this.version++;
      void this.load();
    },
    fetchRange: async (after: string, limit: number) => {
      const response = await eventsRange({ scope: "schedules", after, limit });
      return rangeOf(response.data, after);
    }
  };
}
