import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppliedEvent, ScopeApplier } from "#lib/sync/scope.js";

const api = vi.hoisted(() => ({
  listSchedules: vi.fn(),
  eventsRange: vi.fn(),
}));

vi.mock("#lib/api/client.js", () => api);

const { SchedulesStore } = await import("#lib/stores/schedules.svelte.js");

const schedule = (id: string) => ({ id }) as never;
const changed = [{ event: { kind: "schedule_changed", payload: {} }, overlap: false }] as unknown as AppliedEvent[];

function opened(): { store: InstanceType<typeof SchedulesStore>; applier: ScopeApplier } {
  const store = new SchedulesStore();
  let applier: ScopeApplier | undefined;
  store.open({
    subscribe: (_scope, given) => {
      applier = given;
      return { release() {} };
    },
  });
  return { store, applier: applier! };
}

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void; reject(error: unknown): void } {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  api.listSchedules.mockReset();
  api.eventsRange.mockReset();
});

describe("расписания: снимок области", () => {
  it("изменение между двумя запросами снимка не остаётся позади курсора", async () => {
    // Хаб: список и голова журнала; изменение случается сразу после первого запроса снимка.
    const hub = { head: 5, items: ["a"] };
    let calls = 0;
    const between = (): void => {
      calls += 1;
      if (calls === 1) queueMicrotask(() => { hub.head = 6; hub.items = ["a", "b"]; });
    };
    api.listSchedules.mockImplementation(async () => {
      const data = hub.items.map(schedule);
      between();
      return { data };
    });
    api.eventsRange.mockImplementation(async () => {
      const data = { events: [], head: String(hub.head) };
      between();
      return { data };
    });
    const { applier } = opened();
    const snapshot = await applier.loadSnapshot();
    const listed = (snapshot.data as { items: { id: string }[] }).items.map((item) => item.id);
    // Изменение уже в списке, а его событие ещё впереди курсора и придёт подпиской.
    // В обратном порядке было бы наоборот: курсор «6» и список без «b».
    expect(snapshot.cursor).toBe("5");
    expect(listed).toEqual(["a", "b"]);
  });

  it("без головы журнала снимок не удаётся, а не начинает подписку с начала журнала", async () => {
    api.listSchedules.mockResolvedValue({ data: [schedule("a")] });
    api.eventsRange.mockRejectedValue(new Error("network"));
    const { applier } = opened();
    await expect(applier.loadSnapshot()).rejects.toThrow("network");
  });
});

describe("расписания: перечитывание списка по событиям", () => {
  it("ответ более раннего чтения не затирает более свежий список", async () => {
    const first = deferred<{ data: unknown[] }>();
    const second = deferred<{ data: unknown[] }>();
    api.listSchedules.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const { store, applier } = opened();
    applier.applyEvents(changed);
    applier.applyEvents(changed);
    second.resolve({ data: [schedule("new")] });
    await second.promise;
    await Promise.resolve();
    first.resolve({ data: [schedule("old")] });
    await first.promise;
    await Promise.resolve();
    expect(store.items.map((item) => item.id)).toEqual(["new"]);
    expect(store.version).toBe(2);
  });

  it("поздний ответ чтения, начатого до снимка, снимок не затирает", async () => {
    const stale = deferred<{ data: unknown[] }>();
    api.listSchedules.mockReturnValueOnce(stale.promise).mockResolvedValueOnce({ data: [schedule("snapshot")] });
    api.eventsRange.mockResolvedValue({ data: { events: [], head: "9" } });
    const { store, applier } = opened();
    void store.load();
    const snapshot = await applier.loadSnapshot();
    applier.applySnapshot(snapshot.data, snapshot.cursor);
    stale.resolve({ data: [schedule("old")] });
    await stale.promise;
    await Promise.resolve();
    expect(store.items.map((item) => item.id)).toEqual(["snapshot"]);
  });

  it("неудача более раннего чтения не ставит ошибку поверх свежего списка", async () => {
    const first = deferred<{ data: unknown[] }>();
    api.listSchedules.mockReturnValueOnce(first.promise).mockResolvedValueOnce({ data: [schedule("new")] });
    const { store } = opened();
    const early = store.load();
    await store.load();
    first.reject(new Error("network"));
    await early;
    expect(store.items.map((item) => item.id)).toEqual(["new"]);
    expect(store.error).toBe("");
  });
});
