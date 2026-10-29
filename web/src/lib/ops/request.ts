import type { Op } from "#lib/ops/journal.js";

/** Куда журнал кладёт операцию и каким ключом её повторяет после перезагрузки. */
export function opRequest(
  op: Pick<Op, "kind" | "key" | "conversationId" | "body">,
): {
  url: string;
  init: RequestInit;
} {
  const conversation = op.conversationId ?? "_";
  let url: string;
  let bodyPayload: unknown = op.body ?? {};

  if (op.kind === "create_conversation") {
    url = "/api/v1/conversations";
  } else if (op.kind === "scheduled") {
    url = `/api/v1/conversations/${conversation}/scheduled`;
  } else if (
    op.kind === "other" &&
    op.body &&
    typeof op.body === "object" &&
    "requestId" in op.body
  ) {
    const decisionBody = op.body as { requestId: string; decision?: unknown };
    url = `/api/v1/conversations/${encodeURIComponent(conversation)}/requests/${encodeURIComponent(decisionBody.requestId)}/decision`;
    bodyPayload = decisionBody.decision ?? {};
  } else {
    url = `/api/v1/conversations/${conversation}/inputs`;
  }

  return {
    url,
    init: {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "Idempotency-Key": op.key,
        "X-Canterlot-Client": "1",
      },
      body: JSON.stringify(bodyPayload),
    },
  };
}
