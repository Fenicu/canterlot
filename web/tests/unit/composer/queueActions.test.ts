import { describe, it, expect, vi } from "vitest";
import { SvelteMap } from "svelte/reactivity";
import {
  cancelInput,
  retryAsNew,
  returnToDraft,
} from "#lib/timeline/QueueActions.js";
import { bindReturned } from "#lib/composer/returned.js";
import { freshInput, type InputView } from "#lib/stores/inputs.js";
import type { OpJournal, Op } from "#lib/ops/journal.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import type { DraftStore } from "#lib/drafts/store.js";

describe("timeline/QueueActions", () => {
  it("cancel_outcomes_requested_and_lost_follow_delivery_d3", async () => {
    const input: InputView = freshInput("k1", "Текст для отмены", Date.now());
    input.id = "inp-uuid-1";
    input.state = "queued";

    // 1. Hub returns "cancelled" -> outcome 'cancelled', state becomes 'cancelled'
    const resCancelled = await cancelInput(input, {
      clientCancel: async () => ({ data: "cancelled" }),
    });
    expect(resCancelled).toBe("cancelled");
    expect(input.state).toBe("cancelled");
    expect(input.placement).toBe("none");

    // 2. Hub returns { cancel_requested: { cancel_command_id: 'cmd-1' } } -> outcome 'requested', flag set
    const input2: InputView = freshInput("k2", "Второй ввод", Date.now());
    input2.id = "inp-uuid-2";
    input2.state = "on_machine";
    const resRequested = await cancelInput(input2, {
      clientCancel: async () => ({
        data: { cancel_requested: { cancel_command_id: "cmd-1" } },
      }),
    });
    expect(resRequested).toBe("requested");
    expect(input2.cancelRequested).toBe(true);

    // 3. Hub returns { too_late: { state: 'transmitting' } } -> outcome 'lost'
    const input3: InputView = freshInput("k3", "Третий ввод", Date.now());
    input3.id = "inp-uuid-3";
    input3.state = "transmitting";
    const resLost = await cancelInput(input3, {
      clientCancel: async () => ({
        data: { too_late: { state: "transmitting" } },
      }),
    });
    expect(resLost).toBe("lost");
  });

  it("retry_as_new_uses_a_new_key_and_sends_retry_of_after_confirm", async () => {
    const unconfirmedInput: InputView = freshInput(
      "orig-key",
      "Неподтверждённый текст",
      Date.now(),
    );
    unconfirmedInput.id = "inp-unconfirmed-id";
    unconfirmedInput.state = "acceptance_unconfirmed";

    interface SubmitPayload {
      content?: Array<{ text?: string }>;
      retry_of?: string;
      attachment_ids?: string[];
    }

    const captured = {
      body: null as SubmitPayload | null,
      key: undefined as string | undefined,
    };

    const opsMock = {
      begin: vi.fn(async (kind: string, body: unknown, opts: unknown) => {
        captured.body = body as SubmitPayload;
        captured.key = (opts as { key?: string })?.key;
        return { key: captured.key ?? crypto.randomUUID() } as Op;
      }),
      run: vi.fn(async () => undefined),
    } as unknown as OpJournal;

    const chatMock = {
      inputs: new SvelteMap<string, InputView>(),
    };

    // 1. User cancels confirm -> does not send
    const confirmDeclined = vi.fn(async () => false);
    await retryAsNew(unconfirmedInput, {
      ops: opsMock,
      chat: chatMock as unknown as ChatStore,
      conversationId: "conv-1",
      confirm: confirmDeclined,
    });

    expect(confirmDeclined).toHaveBeenCalled();
    expect(opsMock.begin).not.toHaveBeenCalled();

    // 2. User confirms -> sends with new key and retry_of
    const confirmAccepted = vi.fn(async () => true);
    await retryAsNew(unconfirmedInput, {
      ops: opsMock,
      chat: chatMock as unknown as ChatStore,
      conversationId: "conv-1",
      confirm: confirmAccepted,
    });

    expect(confirmAccepted).toHaveBeenCalled();
    expect(opsMock.begin).toHaveBeenCalled();
    expect(captured.key).toBeDefined();
    expect(captured.key).not.toBe("orig-key"); // New idempotency key!
    expect(captured.body?.retry_of).toBe("inp-unconfirmed-id"); // Points to unconfirmed input
    expect(captured.body?.content?.[0]?.text).toBe("Неподтверждённый текст");

    // Local item placed in chat.inputs
    expect(chatMock.inputs.has(captured.key!)).toBe(true);
    expect(chatMock.inputs.get(captured.key!)?.retryOf).toBe(
      "inp-unconfirmed-id",
    );
  });

  it("return_to_draft_keeps_the_typed_text_and_the_files_in_the_mounted_composer", async () => {
    const attachment = {
      attachment_id: "att-1",
      name: "отчёт.pdf",
      mime: "application/pdf",
      size: 10,
      sha256: "",
      image: false,
    };
    const rejected = freshInput("rej-key", "Отклонённый текст", 1, [attachment]);
    const remove = vi.fn();
    const chatMock = {
      inputs: new SvelteMap<string, InputView>([["rej-key", rejected]]),
      timeline: { remove },
    };
    const take = vi.fn();
    const stop = bindReturned("conv-1", take);

    await returnToDraft(rejected, {
      drafts: {} as DraftStore,
      conversationId: "conv-1",
      chat: chatMock as unknown as ChatStore,
    });
    stop();

    expect(take).toHaveBeenCalledWith({
      text: "Отклонённый текст",
      attachments: [{ id: "att-1", name: "отчёт.pdf", size: 10 }],
    });
    expect(chatMock.inputs.has("rej-key")).toBe(false);
    expect(remove).toHaveBeenCalledWith("input:rej-key");
  });

  it("return_to_draft_without_a_composer_prepends_to_the_draft", async () => {
    const rejected = freshInput("rej-key", "Отклонённый текст", 1);
    const chatMock = {
      inputs: new SvelteMap<string, InputView>([["rej-key", rejected]]),
      timeline: { remove: vi.fn() },
    };
    const drafts = {
      prepend: vi.fn(async () => undefined),
    } as unknown as DraftStore;

    await returnToDraft(rejected, {
      drafts,
      conversationId: "conv-1",
      chat: chatMock as unknown as ChatStore,
    });

    expect(drafts.prepend).toHaveBeenCalledWith(
      "conv-1",
      "Отклонённый текст",
      [],
    );
  });
});

describe("timeline/QueueActions sendNow", () => {
  it("send_now_posts_to_the_running_turn_and_maps_refusal_reasons", async () => {
    const { sendNow, sendNowCaption } = await import("#lib/timeline/QueueActions.js");
    const { ApiError } = await import("#lib/api/http.js");
    const input: InputView = freshInput("k1", "В ход", Date.now());
    input.id = "inp-uuid-1";
    input.state = "queued";

    const posted: Array<[string, string, string]> = [];
    const res = await sendNow(input, "conv-1", {
      clientSendNow: async (conv, id, key) => {
        posted.push([conv, id, key]);
        return { data: { requested: {} } };
      },
    });
    expect(res).toBe("requested");
    expect(posted).toHaveLength(1);
    expect(posted[0]![0]).toBe("conv-1");
    expect(posted[0]![1]).toBe("inp-uuid-1");

    const refused = await sendNow(input, "conv-1", {
      clientSendNow: async () => {
        throw new ApiError(400, "invalid_request", "Некорректный запрос", "browser", "never", "c", {
          reason: "request_open",
        });
      },
    });
    expect(refused).toEqual({ reason: "request_open" });
    expect(sendNowCaption("request_open")).toBe("Дождитесь ответа на запрос");
    expect(sendNowCaption("compacting")).toBe("Дождитесь конца сжатия");
    expect(sendNowCaption("no_running_turn")).toBe("Ход уже кончился: сообщение уйдёт следующим");
    expect(sendNowCaption("capability_missing")).toBeNull();

    const idless: InputView = freshInput("k2", "Черновик", Date.now());
    expect(await sendNow(idless, "conv-1")).toBe("noop");
  });
});

describe("timeline/QueueActions editInput", () => {
  it("edit_posts_text_attachments_and_revision_and_maps_refusals", async () => {
    const { editInput, editInputCaption } = await import("#lib/timeline/QueueActions.js");
    const { ApiError } = await import("#lib/api/http.js");
    const input: InputView = freshInput("k1", "Старый текст", Date.now());
    input.id = "inp-uuid-1";
    input.state = "queued";
    input.revision = 2;

    const posted: Array<{ text: string; attachment_ids: string[]; expected_revision: number }> = [];
    const res = await editInput(input, "conv-1", "Новый текст", ["a1"], 2, {
      clientEditInput: async (_conv, _id, body) => {
        posted.push(body);
        return { data: { input_id: "inp-uuid-1", revision: 3 } };
      },
    });
    expect(res).toBe("saved");
    expect(posted).toHaveLength(1);
    expect(posted[0]).toEqual({ text: "Новый текст", attachment_ids: ["a1"], expected_revision: 2 });

    const stale = await editInput(input, "conv-1", "Поздно", [], 1, {
      clientEditInput: async () => {
        throw new ApiError(400, "invalid_request", "Некорректный запрос", "browser", "never", "c", {
          reason: "revision_conflict",
        });
      },
    });
    expect(stale).toEqual({ reason: "revision_conflict" });
    expect(editInputCaption("revision_conflict")).toBe("Сообщение уже изменено в другом месте");
    expect(editInputCaption("input_not_queued")).toBe("Сообщение уже ушло, править поздно");
    expect(editInputCaption("steer_in_flight")).toBe("Сообщение вливается в ход, дождитесь конца хода");
    expect(editInputCaption("capability_missing")).toBe("Машина сейчас не принимает вложения");

    const idless: InputView = freshInput("k2", "Черновик", Date.now());
    expect(await editInput(idless, "conv-1", "x", [], 1)).toBe("noop");
  });

  it("revision_conflict_is_saved_only_when_text_and_ordered_attachments_match", async () => {
    const { editInput } = await import("#lib/timeline/QueueActions.js");
    const { ApiError } = await import("#lib/api/http.js");
    const conflict = async () => {
      throw new ApiError(409, "invalid_state", "Действие недопустимо", "delivery", "never", "c", {
        reason: "revision_conflict",
      });
    };
    const attachment = (id: string) => ({
      attachment_id: id,
      name: id,
      mime: "text/plain",
      size: 1,
      sha256: "",
      image: false,
    });
    const input: InputView = freshInput("k1", "Тот же текст", Date.now(), [
      attachment("a1"),
      attachment("b2"),
    ]);
    input.id = "inp-uuid-1";
    input.state = "queued";
    input.revision = 3;

    const same = await editInput(input, "conv-1", "Тот же текст", ["a1", "b2"], 2, {
      clientEditInput: conflict,
    });
    expect(same).toBe("saved");

    const reordered = await editInput(input, "conv-1", "Тот же текст", ["b2", "a1"], 2, {
      clientEditInput: conflict,
    });
    expect(reordered).toEqual({ reason: "revision_conflict" });

    const changed = await editInput(input, "conv-1", "Тот же текст", ["a1"], 2, {
      clientEditInput: conflict,
    });
    expect(changed).toEqual({ reason: "revision_conflict" });
  });
});
