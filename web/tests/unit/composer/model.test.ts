import { describe, it, expect, vi } from "vitest";
import { SvelteMap } from "svelte/reactivity";
import {
  ComposerModel,
  type ComposerDeps,
} from "#lib/composer/model.svelte.js";
import type { InputView } from "#lib/stores/inputs.js";
import type { Op, OpJournal, OpKind } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { UploadManager, Upload } from "#lib/uploads/manager.js";
import type { ConnectionStatus } from "#lib/sync/connection.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";

function createMockDeps(overrides?: Partial<ComposerDeps>): {
  deps: ComposerDeps;
  inputs: Map<string, InputView>;
  opsMap: Map<string, Op>;
  persistedKeys: string[];
  draftsMap: Map<string, { text: string; attachments?: unknown[] }>;
  uploadsList: Upload[];
  marks: string[];
} {
  const inputs = new SvelteMap<string, InputView>();
  const opsMap = new Map<string, Op>();
  const persistedKeys: string[] = [];
  const draftsMap = new Map<
    string,
    { text: string; attachments?: unknown[] }
  >();
  const uploadsList: Upload[] = [];
  const marks: string[] = [];

  const chatMock = {
    inputs,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [], run_slot: null },
    activity: { state: "idle" },
  };

  const opsMock = {
    ops: opsMap,
    begin: vi.fn(
      async (
        kind: string,
        body: unknown,
        opts: { conversationId?: string; persist: boolean; key?: string },
      ) => {
        const key = opts.key ?? crypto.randomUUID();
        persistedKeys.push(key);
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
    settle: vi.fn((key: string) => {
      opsMap.delete(key);
    }),
    retryNow: vi.fn((key: string) => {
      const op = opsMap.get(key);
      if (op) op.state = "unknown";
    }),
  } as unknown as OpJournal;

  const draftsMock = {
    set: vi.fn(
      (convId: string, patch: { text?: string; attachments?: unknown[] }) => {
        const cur = draftsMap.get(convId) ?? { text: "" };
        draftsMap.set(convId, { ...cur, ...patch });
      },
    ),
    load: vi.fn(async (convId: string) => draftsMap.get(convId) ?? null),
    clear: vi.fn(async (convId: string) => {
      draftsMap.delete(convId);
    }),
    flush: vi.fn(async () => undefined),
    watchComposer: vi.fn(() => () => undefined),
  } as unknown as DraftStore;

  const uploadsMock = {
    list: vi.fn(() => uploadsList),
    add: vi.fn(),
    cancel: vi.fn(),
    detach: vi.fn((ids: readonly string[]) => {
      const taken = uploadsList.filter((item) => ids.includes(item.id));
      for (const item of taken) uploadsList.splice(uploadsList.indexOf(item), 1);
      return taken;
    }),
    reattach: vi.fn((views: readonly Upload[]) => {
      uploadsList.push(...views);
    }),
  } as unknown as UploadManager;

  const config = vi.fn(() => ({
    max_attachment_bytes: 104_857_600,
    max_attachments_per_input: 10,
    max_input_text_bytes: 262_144,
    queue_limit: 100,
    upload_chunk_bytes: 8_388_608,
  }));

  const canSend = vi.fn(async () => "ok" as const);
  const connection = vi.fn<() => ConnectionStatus>(() => ({
    phase: "live",
    since: 0,
    lastFrameAt: 0,
    attempt: 0,
    resyncs: 0,
  }));

  const probe = {
    mark: vi.fn((name: string) => marks.push(name)),
  };

  const deps: ComposerDeps = {
    conversationId: "conv-1",
    chat: chatMock as unknown as ChatStore,
    ops: opsMock,
    drafts: draftsMock,
    uploads: uploadsMock,
    config,
    canSend,
    connection,
    probe,
    ...overrides,
  };

  return { deps, inputs, opsMap, persistedKeys, draftsMap, uploadsList, marks };
}

describe("composer/model", () => {
  it("gate_reasons_are_russian_text_for_every_disabled_state", () => {
    const { deps, uploadsList } = createMockDeps();
    const model = new ComposerModel(deps);

    // 1. Empty text & no attachments
    model.text = "";
    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      expect(model.gate.reason).toBe("Введите сообщение или прикрепите файл");
    }

    // Only whitespace is treated as empty
    model.text = "   \n  ";
    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      expect(model.gate.reason).toBe("Введите сообщение или прикрепите файл");
    }

    // 2. Attachments uploading
    model.text = "Привет";
    uploadsList.push(
      {
        id: "1",
        conversationId: "conv-1",
        name: "a.png",
        size: 100,
        phase: "uploading",
        sent: 50,
      },
      {
        id: "2",
        conversationId: "conv-1",
        name: "b.png",
        size: 100,
        phase: "ready",
        sent: 100,
      },
      {
        id: "3",
        conversationId: "conv-1",
        name: "c.png",
        size: 100,
        phase: "queued",
        sent: 0,
      },
    );
    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      // 2 uploading out of 3 total
      expect(model.gate.reason).toBe("Файлы ещё загружаются (2 из 3)");
    }

    // When all are ready, gate is ok
    uploadsList[0]!.phase = "ready";
    uploadsList[2]!.phase = "ready";
    expect(model.gate.ok).toBe(true);

    // 3. Text over max_input_text_bytes (256 KiB = 262144 bytes)
    uploadsList.length = 0;
    model.text = "x".repeat(262_145);
    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      expect(model.gate.reason).toBe("Текст длиннее 256 КиБ");
    }

    // Multi-byte Cyrillic text over 256 KiB
    model.text = "я".repeat(131_073); // 131073 * 2 bytes = 262146 bytes > 262144
    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      expect(model.gate.reason).toBe("Текст длиннее 256 КиБ");
    }

    // 4. Queue at limit
    model.text = "Нормальный текст";
    deps.chat.queue.queued = 100;
    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      expect(model.gate.reason).toBe(
        "Очередь заполнена (100): дождитесь выполнения",
      );
    }

    // When queue decreases, gate opens
    deps.chat.queue.queued = 99;
    expect(model.gate.ok).toBe(true);
  });

  it("sending_creates_local_item_then_persists_then_clears_in_that_order", async () => {
    const steps: string[] = [];
    const { deps, inputs } = createMockDeps();

    // Intercept operations to track ordering
    const originalSet = inputs.set.bind(inputs);
    inputs.set = vi.fn((k, v) => {
      steps.push("local_item");
      return originalSet(k, v);
    });

    const originalBegin = deps.ops.begin;
    deps.ops.begin = vi.fn(async (kind, body, opts) => {
      steps.push("persist");
      return originalBegin(kind, body, opts);
    });

    const model = new ComposerModel(deps);
    model.text = "Текст для проверки порядка";

    const originalClear = deps.drafts.clear;
    deps.drafts.clear = vi.fn(async (id) => {
      // Clear happens after persist and empties the textarea
      if (model.text === "") {
        steps.push("clear");
      }
      return originalClear(id);
    });

    const originalRun = deps.ops.run;
    deps.ops.run = vi.fn(async (op) => {
      steps.push("run");
      return originalRun(op);
    });

    await model.send();

    expect(steps).toEqual(["local_item", "persist", "clear", "run"]);
    expect(deps.probe?.mark).toHaveBeenCalledWith("send_pressed");
    expect(deps.probe?.mark).toHaveBeenCalledWith("input_painted");
    expect(model.text).toBe("");
  });

  it("field_clears_with_the_local_item_before_the_journal_write_finishes", async () => {
    const { deps, inputs } = createMockDeps();
    let release: (() => void) | undefined;
    const written = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalBegin = deps.ops.begin;
    deps.ops.begin = vi.fn(async (kind, body, opts) => {
      await written;
      return originalBegin(kind, body, opts);
    });
    const model = new ComposerModel(deps);
    model.text = "Поле свободно сразу";

    const sending = model.send();
    await Promise.resolve();
    await Promise.resolve();

    expect(inputs.size).toBe(1);
    expect([...inputs.values()][0]?.text).toBe("Поле свободно сразу");
    expect(model.text).toBe("");
    expect(deps.drafts.clear).not.toHaveBeenCalled();
    expect(deps.ops.run).not.toHaveBeenCalled();

    release?.();
    await sending;
    expect(deps.drafts.clear).toHaveBeenCalledWith("conv-1");
    expect(deps.ops.run).toHaveBeenCalledTimes(1);
  });

  it("run_starts_without_waiting_for_the_draft_deletion", async () => {
    const { deps } = createMockDeps();
    deps.drafts.clear = vi.fn(() => new Promise<void>(() => undefined));
    const model = new ComposerModel(deps);
    model.text = "Черновик удаляется параллельно";

    await model.send();

    expect(deps.drafts.clear).toHaveBeenCalledWith("conv-1");
    expect(deps.ops.run).toHaveBeenCalledTimes(1);
  });

  it("persist_failure_keeps_text_typed_after_the_press", async () => {
    const { deps, inputs } = createMockDeps();
    let fail: ((error: Error) => void) | undefined;
    deps.ops.begin = vi.fn(
      () =>
        new Promise<Op>((_, reject) => {
          fail = reject;
        }),
    ) as unknown as OpJournal["begin"];
    const model = new ComposerModel(deps);
    model.text = "Первое";

    const sending = model.send();
    await Promise.resolve();
    await Promise.resolve();
    model.text = "Второе";
    fail?.(new Error("IndexedDB disk full"));

    await expect(sending).rejects.toThrow();
    expect(model.text).toBe("Первое\nВторое");
    expect(model.errorMessage).toBe("Не удалось сохранить на устройстве");
    expect(inputs.size).toBe(0);
  });

  it("persist_failure_returns_the_text_with_a_message", async () => {
    const { deps, inputs } = createMockDeps();
    deps.ops.begin = vi.fn(async () => {
      throw new Error("IndexedDB disk full");
    });

    const model = new ComposerModel(deps);
    model.text = "Ценный текст не должен пропасть";

    await expect(model.send()).rejects.toThrow();

    // Text returned to composer
    expect(model.text).toBe("Ценный текст не должен пропасть");
    expect(model.errorMessage).toBe("Не удалось сохранить на устройстве");
    // Local item cleaned up
    expect(inputs.size).toBe(0);
  });

  it("refused_send_returns_text_to_the_draft_and_drops_the_key", async () => {
    const { deps, inputs, draftsMap } = createMockDeps();
    deps.ops.run = vi.fn(async (op) => {
      op.state = "refused";
      op.refusal = { code: "resource_limit", message: "Лимит исчерпан" };
      throw new Error("Refused by hub");
    });

    const model = new ComposerModel(deps);
    model.text = "Сообщение, которое отклонят";

    await model.send();

    // Text returned to composer & draft
    expect(model.text).toBe("Сообщение, которое отклонят");
    expect(draftsMap.get("conv-1")?.text).toBe("Сообщение, которое отклонят");
    // Key dropped from active inputs
    expect(inputs.size).toBe(0);
  });

  it("text_typed_during_the_journal_write_keeps_its_draft", async () => {
    const { deps } = createMockDeps();
    const begin = deps.ops.begin;
    const model = new ComposerModel(deps);
    deps.ops.begin = vi.fn(async (...args: Parameters<OpJournal["begin"]>) => {
      model.text = "Второе";
      return begin.apply(deps.ops, args);
    }) as unknown as OpJournal["begin"];
    model.text = "Первое";

    await model.send();

    expect(model.text).toBe("Второе");
    expect(deps.drafts.clear).not.toHaveBeenCalled();
  });

  it("refused_send_keeps_text_typed_after_the_press", async () => {
    const { deps, draftsMap } = createMockDeps();
    const model = new ComposerModel(deps);
    deps.ops.run = vi.fn(async (op) => {
      model.text = "Второе";
      op.state = "refused";
      op.refusal = { code: "resource_limit", message: "Лимит исчерпан" };
    });
    model.text = "Первое";

    await model.send();

    expect(model.text).toBe("Первое\nВторое");
    expect(draftsMap.get("conv-1")?.text).toBe("Первое\nВторое");
  });

  it("sent_uploads_leave_the_tray_without_a_server_delete_and_return_on_refusal", async () => {
    const { deps, uploadsList } = createMockDeps();
    const ready = (id: string): Upload => ({
      id,
      conversationId: "conv-1",
      name: `${id}.txt`,
      size: 3,
      phase: "ready",
      sent: 3,
      attachmentId: `att-${id}`,
    });
    uploadsList.push(ready("u1"));
    const model = new ComposerModel(deps);
    model.text = "с файлом";
    await model.send();
    expect(deps.ops.begin).toHaveBeenLastCalledWith(
      "input",
      expect.objectContaining({ attachment_ids: ["att-u1"] }),
      expect.anything(),
    );
    expect(uploadsList).toEqual([]);
    expect(deps.uploads.cancel).not.toHaveBeenCalled();

    model.text = "без файла";
    await model.send();
    expect(deps.ops.begin).toHaveBeenLastCalledWith(
      "input",
      expect.objectContaining({ attachment_ids: undefined }),
      expect.anything(),
    );

    uploadsList.push(ready("u2"));
    deps.ops.run = vi.fn(async (op: Op) => {
      op.state = "refused";
      op.refusal = { code: "resource_limit", message: "Лимит исчерпан" };
      throw new Error("refused");
    });
    model.text = "отклонят";
    await model.send();
    expect(uploadsList.map((item) => item.id)).toEqual(["u2"]);
  });

  it("second_tap_during_the_network_check_creates_no_second_input", async () => {
    const gates: ((value: "ok") => void)[] = [];
    const { deps } = createMockDeps({
      canSend: vi.fn(
        () => new Promise<"ok">((resolve) => gates.push(resolve)),
      ),
    });
    const clock = vi.spyOn(Date, "now").mockReturnValue(1_000_000);
    try {
      const model = new ComposerModel(deps);
      model.text = "один раз";
      const first = model.send();
      clock.mockReturnValue(1_000_700);
      const second = model.send();
      for (const open of gates.splice(0)) open("ok");
      await Promise.all([first, second]);
      expect(deps.ops.begin).toHaveBeenCalledTimes(1);

      model.text = "позже";
      const later = model.sendLater(1_000_700 + 15 * 60_000);
      const again = model.sendLater(1_000_700 + 15 * 60_000);
      for (const open of gates.splice(0)) open("ok");
      await Promise.all([later, again]);
      expect(deps.ops.begin).toHaveBeenCalledTimes(2);
    } finally {
      clock.mockRestore();
    }
  });

  it("send_reports_busy_until_the_network_check_and_persist_finish", async () => {
    let open!: (value: "ok") => void;
    const { deps } = createMockDeps({
      canSend: vi.fn(() => new Promise<"ok">((resolve) => (open = resolve))),
    });
    const model = new ComposerModel(deps);
    model.text = "занят";
    const sending = model.send();
    expect(model.sending).toBe(true);
    open("ok");
    await sending;
    expect(model.sending).toBe(false);
  });

  it("text_typed_during_the_network_check_stays_in_the_composer", async () => {
    let open!: (value: "ok") => void;
    const { deps } = createMockDeps({
      canSend: vi.fn(() => new Promise<"ok">((resolve) => (open = resolve))),
    });
    const model = new ComposerModel(deps);
    model.text = "отправлено";
    const sending = model.send();
    model.text = "отправлено и дописано";
    open("ok");
    await sending;
    expect(deps.ops.begin).toHaveBeenCalledWith(
      "input",
      expect.objectContaining({
        content: [{ type: "text", text: "отправлено" }],
      }),
      expect.anything(),
    );
    expect(model.text).toBe("отправлено и дописано");
  });

  it("text_typed_after_the_press_stays_out_of_the_sent_message", async () => {
    let open!: (value: "ok") => void;
    const { deps } = createMockDeps({
      canSend: vi.fn(() => new Promise<"ok">((resolve) => (open = resolve))),
    });
    const model = new ComposerModel(deps);
    model.text = "foo";

    const sending = model.send();
    expect(model.text).toBe("");
    model.text = " bar";
    open("ok");
    await sending;

    expect(deps.ops.begin).toHaveBeenCalledWith(
      "input",
      expect.objectContaining({ content: [{ type: "text", text: "foo" }] }),
      expect.anything(),
    );
    expect(model.text).toBe(" bar");
    expect(deps.drafts.clear).not.toHaveBeenCalled();
  });

  it("offline_after_the_press_returns_the_text_before_what_was_typed", async () => {
    let open!: (value: "offline") => void;
    const { deps, draftsMap } = createMockDeps({
      canSend: vi.fn(
        () => new Promise<"offline">((resolve) => (open = resolve)),
      ),
    });
    const model = new ComposerModel(deps);
    model.text = "Первое";

    const sending = model.send();
    model.text = "Второе";
    open("offline");
    await sending;

    expect(model.text).toBe("Первое\nВторое");
    expect(draftsMap.get("conv-1")?.text).toBe("Первое\nВторое");
    expect(deps.ops.begin).not.toHaveBeenCalled();
  });

  it("queue_filled_during_the_network_check_returns_the_text", async () => {
    let open!: (value: "ok") => void;
    const { deps } = createMockDeps({
      canSend: vi.fn(() => new Promise<"ok">((resolve) => (open = resolve))),
    });
    const model = new ComposerModel(deps);
    model.text = "Очередь заполнится";

    const sending = model.send();
    deps.chat.queue.queued = 100;
    open("ok");
    await sending;

    expect(model.text).toBe("Очередь заполнится");
    expect(deps.ops.begin).not.toHaveBeenCalled();
  });

  it("send_frees_the_button_once_journaled_while_the_post_hangs", async () => {
    const { deps } = createMockDeps();
    deps.ops.run = vi.fn(() => new Promise<void>(() => undefined));
    const model = new ComposerModel(deps);
    model.text = "Первое";

    void model.send();
    await vi.waitFor(() => expect(deps.ops.run).toHaveBeenCalledTimes(1));
    expect(model.sending).toBe(false);

    model.text = "Второе";
    void model.send();
    await vi.waitFor(() => expect(deps.ops.run).toHaveBeenCalledTimes(2));
    const keys = vi
      .mocked(deps.ops.begin)
      .mock.calls.map((call) => call[2].key);
    expect(new Set(keys).size).toBe(2);
    expect(model.text).toBe("");
  });

  it("finished_delivery_does_not_unlock_the_next_press", async () => {
    let finish!: () => void;
    let open!: (value: "ok") => void;
    const { deps } = createMockDeps();
    deps.ops.run = vi.fn(
      () => new Promise<void>((resolve) => (finish = resolve)),
    );
    const model = new ComposerModel(deps);
    model.text = "Первое";
    const first = model.send();
    await vi.waitFor(() => expect(deps.ops.run).toHaveBeenCalledTimes(1));

    deps.canSend = vi.fn(
      () => new Promise<"ok">((resolve) => (open = resolve)),
    );
    model.text = "Второе";
    const second = model.send();
    finish();
    await first;

    expect(model.sending).toBe(true);
    void model.send();
    open("ok");
    await vi.waitFor(() => expect(deps.ops.run).toHaveBeenCalledTimes(2));
    finish();
    await second;
    expect(deps.ops.begin).toHaveBeenCalledTimes(2);
  });

  it("send_later_clears_the_field_on_press_and_keeps_what_was_typed", async () => {
    let open!: (value: "ok") => void;
    const { deps } = createMockDeps({
      canSend: vi.fn(() => new Promise<"ok">((resolve) => (open = resolve))),
      serverNow: () => 1_000_000,
    });
    const model = new ComposerModel(deps);
    model.text = "foo";

    const sending = model.sendLater(1_000_000 + 15 * 60_000);
    expect(model.text).toBe("");
    model.text = "bar";
    open("ok");
    await sending;

    expect(deps.ops.begin).toHaveBeenCalledWith(
      "scheduled",
      expect.objectContaining({ content: [{ type: "text", text: "foo" }] }),
      expect.anything(),
    );
    expect(model.text).toBe("bar");
    expect(deps.drafts.clear).not.toHaveBeenCalled();
  });

  it("send_later_keeps_the_text_when_the_journal_write_fails", async () => {
    const { deps } = createMockDeps({ serverNow: () => 1_000_000 });
    deps.ops.begin = vi.fn(async () => {
      throw new Error("IndexedDB disk full");
    });
    const model = new ComposerModel(deps);
    model.text = "Позже";

    await expect(
      model.sendLater(1_000_000 + 15 * 60_000),
    ).rejects.toThrow();

    expect(model.text).toBe("Позже");
    expect(model.errorMessage).toBe("Не удалось сохранить на устройстве");
  });

  it("refused_send_later_returns_the_text_the_draft_and_the_files", async () => {
    const { deps, draftsMap, uploadsList } = createMockDeps({
      serverNow: () => 1_000_000,
    });
    uploadsList.push({
      id: "u1",
      conversationId: "conv-1",
      name: "a.txt",
      size: 3,
      phase: "ready",
      sent: 3,
      attachmentId: "att-u1",
    });
    deps.ops.run = vi.fn(async (op: Op) => {
      op.state = "refused";
      op.refusal = { code: "too_many_scheduled", message: "Слишком много" };
      throw new Error("refused");
    });
    const model = new ComposerModel(deps);
    model.text = "Длинное сообщение на потом";

    await model.sendLater(1_000_000 + 15 * 60_000);

    expect(model.text).toBe("Длинное сообщение на потом");
    expect(draftsMap.get("conv-1")?.text).toBe("Длинное сообщение на потом");
    expect(uploadsList.map((item) => item.id)).toEqual(["u1"]);
    expect(model.errorMessage).toBe("Слишком много");
  });

  it("taking_a_message_back_prepends_it_and_restores_the_files", () => {
    const { deps, draftsMap } = createMockDeps();
    deps.uploads.restore = vi.fn();
    const model = new ComposerModel(deps);
    model.text = "набираю";

    model.takeBack({
      text: "вернулось",
      attachments: [{ id: "att-1" }],
      notice: "Не доставлено",
    });

    expect(model.text).toBe("вернулось\nнабираю");
    expect(draftsMap.get("conv-1")?.text).toBe("вернулось\nнабираю");
    expect(deps.uploads.restore).toHaveBeenCalledWith("conv-1", [
      { id: "att-1" },
    ]);
    expect(model.errorMessage).toBe("Не доставлено");
  });

  it("offline_press_keeps_a_draft_and_creates_no_operation", async () => {
    const { deps, inputs, draftsMap } = createMockDeps({
      canSend: vi.fn(async () => "offline" as const),
    });

    const model = new ComposerModel(deps);
    model.text = "Черновик без сети";

    await model.send();

    expect(model.errorMessage).toBe(
      "Нет связи: сообщение сохранено как черновик и не отправлено",
    );
    expect(model.text).toBe("Черновик без сети");
    expect(draftsMap.get("conv-1")?.text).toBe("Черновик без сети");
    // No operation created in journal
    expect(deps.ops.begin).not.toHaveBeenCalled();
    // No local item placed in chat inputs
    expect(inputs.size).toBe(0);
  });

  it("offline_gate_ignores_wss_liveness", async () => {
    const { deps } = createMockDeps({
      // WSS connection is closed / offline
      connection: vi.fn<() => ConnectionStatus>(() => ({
        phase: "offline",
        since: 1000,
        lastFrameAt: 500,
        attempt: 5,
        resyncs: 2,
      })),
      // HTTP check is OK
      canSend: vi.fn(async () => "ok" as const),
    });

    const model = new ComposerModel(deps);
    model.text = "Отправка работает без WSS через HTTP";

    await model.send();

    // Sending must succeed: begin and run called
    expect(deps.ops.begin).toHaveBeenCalled();
    expect(deps.ops.run).toHaveBeenCalled();
    expect(model.text).toBe("");
  });

  it("unknown_operation_shows_retry_state_and_reuses_the_key", async () => {
    const { deps, opsMap } = createMockDeps();
    let createdOpKey = "";
    deps.ops.run = vi.fn(async (op) => {
      op.state = "unknown";
      createdOpKey = op.key;
    });

    const model = new ComposerModel(deps);
    model.text = "Сетевой сбой при отправке";

    await model.send();

    const op = opsMap.get(createdOpKey);
    expect(op).toBeDefined();
    expect(op?.state).toBe("unknown");

    // Retrying reuses the same key
    deps.ops.retryNow(createdOpKey);
    expect(deps.ops.retryNow).toHaveBeenCalledWith(createdOpKey);
    expect(opsMap.get(createdOpKey)?.key).toBe(createdOpKey);
  });

  it("queue_full_keeps_the_text_and_explains_the_limit", async () => {
    const { deps } = createMockDeps();
    deps.chat.queue.queued = 100;

    const model = new ComposerModel(deps);
    model.text = "Очередь заполнена, текст должен сохраниться";

    expect(model.gate.ok).toBe(false);
    if (!model.gate.ok) {
      expect(model.gate.reason).toBe(
        "Очередь заполнена (100): дождитесь выполнения",
      );
    }

    await model.send();

    // Text is kept in composer
    expect(model.text).toBe("Очередь заполнена, текст должен сохраниться");
    expect(deps.ops.begin).not.toHaveBeenCalled();
  });

  it("interrupt_label_stays_until_terminal_status", async () => {
    const { deps } = createMockDeps();
    let calls = 0;
    deps.interruptApi = async () => {
      calls += 1;
    };
    const model = new ComposerModel(deps);
    const first = model.interrupt();
    const second = model.interrupt();
    await first;
    await second;
    expect(calls).toBe(1);
    expect(model.interruptLabel).toBe("Прерываю…");
    expect(model.interruptRunning).toBe(true);
    model.applyInterruptStatus("acknowledged");
    expect(model.interruptLabel).toBe("Прерываю…");
    model.applyInterruptStatus("completed");
    expect(model.interruptLabel).toBe("Прервано");
    expect(model.interruptRunning).toBe(false);
    model.applyInterruptStatus("not_applicable");
    expect(model.interruptLabel).toBe("Прерывание не требуется");
    model.applyInterruptStatus("superseded");
    expect(model.interruptLabel).toBe("Прерывание заменено");
    model.applyInterruptStatus("rejected_stale");
    expect(model.interruptLabel).toBe("Прерывание устарело");
  });
});
