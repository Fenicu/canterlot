import { leaveAttachmentNotice } from "#lib/composer/attachmentNotice.js";
import type { DraftAttachment, DraftStore } from "#lib/drafts/store.js";
import type { Op } from "#lib/ops/journal.js";
import { inputOfBody } from "#lib/stores/inputs.js";

export interface Returned {
  text: string;
  attachments: DraftAttachment[];
  notice?: string;
}

const composers = new Map<string, (returned: Returned) => void>();

export function bindReturned(
  conversationId: string,
  take: (returned: Returned) => void,
): () => void {
  composers.set(conversationId, take);
  return () => {
    if (composers.get(conversationId) === take) composers.delete(conversationId);
  };
}

/** Смонтированный композер принимает сообщение сам, иначе оно ждёт в черновике разговора. */
export async function returnToComposer(
  conversationId: string,
  returned: Returned,
  drafts: DraftStore,
): Promise<void> {
  const take = composers.get(conversationId);
  if (take) {
    take(returned);
    return;
  }
  await drafts.prepend(conversationId, returned.text, returned.attachments);
  if (returned.notice) leaveAttachmentNotice(conversationId, returned.notice);
}

/** Строка журнала уже удалена: после перезагрузки это единственная копия сообщения. */
export async function returnRefused(op: Op, drafts: DraftStore): Promise<void> {
  if (!op.conversationId) return;
  if (op.kind !== "input" && op.kind !== "scheduled") return;
  const { text, attachmentIds } = inputOfBody(op.body);
  const reason = op.refusal?.message ?? "отклонено хабом";
  await returnToComposer(
    op.conversationId,
    {
      text,
      attachments: attachmentIds.map((id) => ({ id })),
      notice: `Сообщение не доставлено и возвращено в поле ввода: ${reason}`,
    },
    drafts,
  );
}
