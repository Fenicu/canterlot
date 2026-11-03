import { httpHealth } from "#lib/api/gate.js";
import {
  ApiError,
  NetworkError,
  envelopeMissing,
  envelopeOf,
  screenForeign,
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
  // Переадресацию на вход не проходим: страница входа с кодом 200 иначе выглядела бы принятым сообщением.
  const response = await fetchImpl(request.url, {
    ...request.init,
    credentials: "same-origin",
    redirect: "manual",
    signal,
  }).catch((cause: unknown) => {
    throw new NetworkError("send_failed", { cause });
  });
  try {
    await screenForeign(response, request.url);
  } catch (err) {
    if (signal.aborted) throw new NetworkError("send_failed");
    throw err;
  }
  const contentType = response.headers?.get("Content-Type") ?? "";
  const body: unknown =
    contentType === "" || contentType.includes("application/json")
      ? await response.json().catch(() => undefined)
      : undefined;
  if (signal.aborted) throw new NetworkError("send_failed");
  if (!response.ok) throw httpFailure(response, body);
  // Отправленной операция считается только по JSON-ответу хаба.
  if (body === null || typeof body !== "object")
    throw new NetworkError("invalid_response");
  httpHealth.recordSuccess();
  return body;
}

function httpFailure(response: Response, body: unknown): Error {
  const error = envelopeOf(body);
  if (!error) return envelopeMissing(response.status);
  return new ApiError(
    response.status,
    error.code,
    error.message,
    error.scope,
    error.retry,
    error.correlation_id,
    error.details,
  );
}
