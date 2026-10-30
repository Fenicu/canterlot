import { httpHealth } from "#lib/api/gate.js";
import {
  ApiError,
  NetworkError,
  type ErrorCode,
  type ErrorDetails,
  type ErrorScope,
  type Retry,
} from "#lib/api/http.js";
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

const SEND_TIMEOUT_MS = 30_000;

/** Один запрос журнала. Принятый ответ хаба тоже свежий HTTP: затвор отправки не идёт в `GET /me`. */
export async function sendOp(
  fetchImpl: typeof fetch,
  op: Pick<Op, "kind" | "key" | "conversationId" | "body">,
  timeoutMs = SEND_TIMEOUT_MS,
): Promise<unknown> {
  const request = opRequest(op);
  const signal = AbortSignal.timeout(timeoutMs);
  // Обрыв соединения, потерянный ответ или зависший запрос: операция остаётся «неизвестной» и повторяется с тем же ключом.
  const response = await fetchImpl(request.url, { ...request.init, signal }).catch(
    (cause: unknown) => {
      throw new NetworkError("send_failed", { cause });
    },
  );
  const body = await response.json().catch(() => null);
  if (signal.aborted) throw new NetworkError("send_failed");
  if (!response.ok) throw httpFailure(response, body);
  httpHealth.recordSuccess();
  return body;
}

function httpFailure(response: Response, body: unknown): Error {
  const error = (
    body as {
      error?: {
        code?: string;
        message?: string;
        scope?: string;
        retry?: string;
        correlation_id?: string;
        details?: ErrorDetails;
      };
    } | null
  )?.error;
  if (
    error?.code &&
    error.message &&
    error.scope &&
    error.retry &&
    error.correlation_id
  ) {
    return new ApiError(
      response.status,
      error.code as ErrorCode,
      error.message,
      error.scope as ErrorScope,
      error.retry as Retry,
      error.correlation_id,
      error.details,
    );
  }
  return new NetworkError("send_failed");
}
