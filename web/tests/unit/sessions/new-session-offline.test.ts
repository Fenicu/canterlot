import "fake-indexeddb/auto";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { components } from "#lib/api/generated/openapi.js";
import { closeOpenJournals, OpJournal, type JournalEnv } from "#lib/ops/journal.js";
import {
  CreateCancelled,
  NewSessionModel,
  pendingCreate,
} from "#lib/screens/sessions/newSessionModel.js";
import { openDb, purgeAllStores } from "#lib/storage/idb.js";
import type { MachineRecord, MachinesStore } from "#lib/stores/machines.svelte.js";

vi.mock("#lib/api/client.js", () => ({
  recentWorkdirs: vi.fn(async () => ({ data: { workdirs: [] } })),
}));

const config = {
  defaults: { crunch_resume_text: "продолжи с места остановки" },
} as components["schemas"]["ConfigView"];

const machines = {
  items: new Map([
    [
      "m1",
      {
        id: "m1",
        name: "workstation",
        availability: "ready",
        os: "linux",
        warnings: [],
        metrics: null,
        capabilities: ["pilot_v1"],
        allowed_roots: ["/srv"],
        harnesses: [
          {
            id: "claude",
            title: "Claude Code",
            provider: "claude",
            models: [{ id: "opus", title: "opus", efforts: ["low", "medium"] }],
            permission_modes: ["owner"],
          },
        ],
      } as unknown as MachineRecord,
    ],
  ]),
} as unknown as MachinesStore;

/** Журнал, у которого связь включает и выключает тест; отложенные проверки связи тест запускает сам. */
async function journalWith(online: () => boolean) {
  closeOpenJournals();
  await purgeAllStores();
  const db = await openDb("new-session-offline");
  const scheduled = new Set<() => void>();
  const env: JournalEnv = {
    now: () => 0,
    online,
    authExpired: () => false,
    schedule: (fn) => {
      scheduled.add(fn);
      return () => scheduled.delete(fn);
    },
  };
  /** Время прошло: журнал перепроверяет связь. */
  const elapse = () => {
    for (const fn of [...scheduled]) {
      scheduled.delete(fn);
      fn();
    }
  };
  const sent: string[] = [];
  const journal = new OpJournal(
    db,
    async (op) => {
      sent.push(op.key);
      return { conversation_id: `conv-${sent.length}` };
    },
    env,
  );
  return { journal, sent, elapse };
}

function form(journal: OpJournal): NewSessionModel {
  const model = new NewSessionModel(machines, config, journal);
  model.machineId = "m1";
  model.workdir = "/srv/canterlot";
  model.harness = "claude";
  model.model = "opus";
  model.effort = "medium";
  return model;
}

describe("sessions/new session without a connection", () => {
  afterEach(() => closeOpenJournals());

  it("a_creation_waiting_for_the_hub_is_visible_to_a_reopened_form", async () => {
    let online = false;
    const { journal, sent, elapse } = await journalWith(() => online);
    const first = form(journal);
    const waiting = first.submit();
    await vi.waitFor(() => expect(pendingCreate(journal.ops.values())?.state).toBe("unknown"));

    // Владелец закрыл форму и открыл её снова: это новая модель, но создание в журнале одно,
    // и форма по нему не даёт нажать «Создать» второй раз.
    const reopened = form(journal);
    expect(reopened).not.toBe(first);
    expect(pendingCreate(journal.ops.values())).not.toBeNull();
    expect(sent).toEqual([]);

    online = true;
    elapse();
    await expect(waiting).resolves.toEqual({ conversationId: "conv-1" });
    expect(sent).toHaveLength(1);
    expect(pendingCreate(journal.ops.values())).toBeNull();
  });

  it("a_cancelled_creation_never_reaches_the_hub_and_the_next_press_is_a_new_one", async () => {
    let online = false;
    const { journal, sent } = await journalWith(() => online);
    const model = form(journal);
    const waiting = model.submit();
    await vi.waitFor(() => expect(pendingCreate(journal.ops.values())?.state).toBe("unknown"));
    const cancelled = pendingCreate(journal.ops.values())!.key;

    journal.discard(cancelled);

    await expect(waiting).rejects.toBeInstanceOf(CreateCancelled);
    expect(pendingCreate(journal.ops.values())).toBeNull();

    online = true;
    await expect(model.submit()).resolves.toEqual({ conversationId: "conv-1" });
    // Отменённая операция не ушла, а новое нажатие получило свой ключ.
    expect(sent).toHaveLength(1);
    expect(sent[0]).not.toBe(cancelled);
  });
});
