import { describe, expect, it, vi } from "vitest";
import {
  bindReturned,
  returnRefused,
} from "#lib/composer/returned.js";
import { takeAttachmentNotice } from "#lib/composer/attachmentNotice.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { Op } from "#lib/ops/journal.js";

function refused(kind: Op["kind"], conversationId = "conv-1"): Op {
  return {
    key: "k",
    kind,
    conversationId,
    body: {
      content: [{ type: "text", text: "текст" }],
      attachment_ids: ["att-1"],
    },
    state: "refused",
    attempts: 1,
    createdAt: 1,
    refusal: { code: "conversation_closed", message: "Разговор закрыт" },
  };
}

function draftsMock(): DraftStore {
  return { prepend: vi.fn(async () => undefined) } as unknown as DraftStore;
}

describe("composer/returned", () => {
  it("without_a_composer_the_message_waits_in_the_draft_with_a_notice", async () => {
    const drafts = draftsMock();

    await returnRefused(refused("input"), drafts);

    expect(drafts.prepend).toHaveBeenCalledWith("conv-1", "текст", [
      { id: "att-1" },
    ]);
    expect(takeAttachmentNotice("conv-1")).toContain("Разговор закрыт");
  });

  it("a_mounted_composer_takes_the_message_itself", async () => {
    const drafts = draftsMock();
    const take = vi.fn();
    const stop = bindReturned("conv-1", take);

    await returnRefused(refused("scheduled"), drafts);
    stop();

    expect(take).toHaveBeenCalledWith(
      expect.objectContaining({
        text: "текст",
        attachments: [{ id: "att-1" }],
      }),
    );
    expect(drafts.prepend).not.toHaveBeenCalled();
    expect(takeAttachmentNotice("conv-1")).toBeNull();
  });

  it("a_refusal_for_a_deleted_session_keeps_nothing_on_the_device", async () => {
    const drafts = {
      prepend: vi.fn(async () => undefined),
      clear: vi.fn(async () => undefined),
    } as unknown as DraftStore;
    const op = refused("input");
    op.refusal = { code: "conversation_deleted", message: "Сессия удалена" };

    await returnRefused(op, drafts);

    expect(drafts.prepend).not.toHaveBeenCalled();
    expect(drafts.clear).toHaveBeenCalledWith("conv-1");
  });

  it("operations_without_a_message_return_nothing", async () => {
    const drafts = draftsMock();

    await returnRefused(refused("create_conversation"), drafts);
    await returnRefused(refused("other"), drafts);

    expect(drafts.prepend).not.toHaveBeenCalled();
  });
});
