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
  /** Номер последнего начатого чтения списка. */
  #reads = 0;
  /** Номер чтения, чей ответ сейчас показан. */
  #shown = 0;

  async load(): Promise<void> {
    const read = ++this.#reads;
    try {
      this.#show(read, (await listSchedules()).data);
    } catch {
      // Отказ раннего чтения не прячет список, который успело принести более позднее.
      if (read < this.#shown) return;
      this.error = "Не удалось загрузить расписания. Повторите при восстановлении связи";
    }
  }

  /**
   * Каждое событие запускает своё чтение, и ответы приходят в любом порядке:
   * ответ чтения, начатого раньше показанного, список не затирает.
   */
  #show(read: number, items: Schedule[]): void {
    if (read < this.#shown) return;
    this.#shown = read;
    this.items = items;
    this.error = "";
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
      // Голова журнала читается раньше списка: изменение между двумя запросами попадает
      // в список, а его событие остаётся впереди курсора и перечитает список ещё раз.
      // В обратном порядке оно оказалось бы позади курсора и не в списке. Отказ запроса
      // головы роняет снимок, и область повторит его: курсор «0» повёл бы подписку
      // с начала журнала.
      const response = await eventsRange({ scope: "schedules", limit: 1 });
      const body = response.data as RangeBody | undefined;
      const cursor = body && typeof body.head === "string" && !("resync_required" in body)
        ? body.head
        : "0";
      const read = ++this.#reads;
      const list = await listSchedules();
      return { cursor, data: { read, items: list.data } };
    },
    applySnapshot: (data: unknown) => {
      const snapshot = data as { read: number; items: Schedule[] };
      this.#show(snapshot.read, snapshot.items ?? []);
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
