import { describe, it, expect, vi, beforeEach } from "vitest";
import { SvelteMap } from "svelte/reactivity";
import {
  ComposerModel,
  type ComposerDeps,
} from "#lib/composer/model.svelte.js";
import type { InputView } from "#lib/stores/inputs.js";
import type { Op, OpJournal, OpKind } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { UploadManager } from "#lib/uploads/manager.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import type { DraftRecord } from "#lib/drafts/sync.js";

/**
 * Переход между сессиями: текст, отправленный в A, не должен появиться
 * в композере B, а черновик A после отправки очищается.
 */

interface ManualTimer {
  at: number;
  fn: () => void;
  cancelled: boolean;
}

function manualClock() {
  let now = 1_000_000;
  const timers: ManualTimer[] = [];
  const schedule = (fn: () => void, ms: number) => {
    const timer: ManualTimer = { at: now + ms, fn, cancelled: false };
    timers.push(timer);
    return () => {
      timer.cancelled = true;
    };
  };
  async function settle(rounds = 30) {
    for (let i = 0; i < rounds; i++) await Promise.resolve();
  }
  async function advance(ms: number) {
    const target = now + ms;
    for (;;) {
      timers.sort((a, b) => a.at - b.at);
      const next = timers.find((t) => !t.cancelled && t.at <= target);
      if (!next) break;
      next.cancelled = true;
      now = next.at;
      next.fn();
      await settle();
    }
    now = target;
    await settle();
  }
  return { schedule, advance, now: () => now };
}

interface StoredDraft {
  text: string;
  attachments: { id: string }[];
  baseRevision?: number;
  unsynced?: boolean;
}

function mockDrafts() {
  const map = new Map<string, StoredDraft>();
  return {
    map,
    store: {
      set: vi.fn((conversationId: string, patch: Partial<StoredDraft>) => {
        const cur = map.get(conversationId) ?? { text: "", attachments: [] };
        map.set(conversationId, { ...cur, ...patch });
      }),
      load: vi.fn(async (conversationId: string) => map.get(conversationId) ?? null),
      clear: vi.fn(async (conversationId: string) => {
        map.delete(conversationId);
      }),
      flush: vi.fn(async () => undefined),
    } as unknown as DraftStore,
  };
}

function fakeHub() {
  const records = new Map<string, DraftRecord>();
  const puts: Array<{ conversationId: string; text: string; base: number }> =
    [];
  return {
    puts,
    apiFor(conversationId: string) {
      return {
        get: async (): Promise<DraftRecord> =>
          records.get(conversationId) ?? {
            text: "",
            attachment_ids: [],
            revision: 0,
          },
        put: async (text: string, attachmentIds: string[], base: number) => {
          puts.push({ conversationId, text, base });
          const revision = (records.get(conversationId)?.revision ?? 0) + 1;
          const record: DraftRecord = {
            text,
            attachment_ids: attachmentIds,
            revision,
            device_id: "device-a",
          };
          records.set(conversationId, record);
          return record;
        },
      };
    },
    record(conversationId: string) {
      return records.get(conversationId) ?? null;
    },
  };
}

function mockChat() {
  const inputs = new SvelteMap<string, InputView>();
  return {
    inputs,
    admitLocal: (view: InputView) => {
      inputs.set(view.key, view);
    },
    discardLocal: (key: string) => {
      inputs.delete(key);
    },
    queue: { queued: 0, pause_reasons: [], hold_reasons: [], run_slot: null },
    activity: { state: "idle" },
  };
}

function mockOps() {
  const opsMap = new Map<string, Op>();
  return {
    ops: opsMap,
    begin: vi.fn(
      async (
        kind: string,
        body: unknown,
        opts: { conversationId?: string; persist: boolean; key?: string },
      ) => {
        const key = opts.key ?? crypto.randomUUID();
        const op: Op = {
          key,
          kind: kind as OpKind,
          conversationId: opts.conversationId,
          body,
          state: "saving",
          attempts: 0,
          createdAt: Date.now(),
        };
        opsMap.set(key, op);
        return op;
      },
    ),
    run: vi.fn(async (op: Op) => {
      op.state = "committed";
    }),
  } as unknown as OpJournal;
}

function mockUploads() {
  return {
    list: vi.fn(() => []),
    add: vi.fn(),
    cancel: vi.fn(async () => undefined),
    detach: vi.fn(() => []),
    reattach: vi.fn(),
    restore: vi.fn(),
  } as unknown as UploadManager;
}

function depsFor(
  conversationId: string,
  hub: ReturnType<typeof fakeHub>,
  drafts: DraftStore,
  clock: ReturnType<typeof manualClock>,
): ComposerDeps {
  return {
    conversationId,
    chat: mockChat() as unknown as ChatStore,
    ops: mockOps(),
    drafts,
    uploads: mockUploads(),
    config: () => ({
      max_attachment_bytes: 104_857_600,
      max_attachments_per_input: 10,
      max_input_text_bytes: 262_144,
      queue_limit: 100,
    }),
    canSend: async () => "ok" as const,
    connection: () => ({ phase: "live", since: 0, lastFrameAt: 0, attempt: 0, resyncs: 0 }),
    serverDrafts: hub.apiFor(conversationId),
    deviceId: "device-a",
    now: clock.now,
    schedule: clock.schedule,
    online: () => true,
  };
}

describe("composer/session-switch", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("sent_text_in_A_stays_out_of_B_composer_and_B_draft", async () => {
    const { store: drafts, map } = mockDrafts();
    const hub = fakeHub();
    const clock = manualClock();

    // Открыта сессия A, серверный черновик пуст.
    const modelA = new ComposerModel(depsFor("conv-A", hub, drafts, clock));
    await modelA.startDraft(hub.record("conv-A"));

    // Владелец печатает и отправляет.
    modelA.text = "сообщение из сессии A";
    modelA.typed();
    await clock.advance(1000);
    await modelA.send();
    await clock.advance(1000);
    expect(modelA.text).toBe("");

    // Уход с экрана A (размонтирование композера).
    await modelA.leaveDraft();

    // Открыта сессия B.
    const modelB = new ComposerModel(depsFor("conv-B", hub, drafts, clock));
    await modelB.startDraft(hub.record("conv-B"));

    expect(modelB.text).toBe("");
    expect(map.get("conv-B")?.text ?? "").toBe("");
    // Черновик A после отправки очищен.
    expect(map.get("conv-A")).toBeUndefined();
  });
});
