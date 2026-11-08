import { clock } from "#lib/clock.js";
import type { components } from "#lib/api/generated/openapi.js";

class HttpHealthTracker {
  #lastOkAt: number | null = null;

  get lastOkAt(): number | null {
    return this.#lastOkAt;
  }

  /** Совместимость с уже написанными проверками. */
  get lastSuccessAt(): number {
    return this.#lastOkAt ?? 0;
  }

  recordSuccess(timestamp = Date.now()): void {
    this.#lastOkAt = timestamp;
  }

  reset(): void {
    this.#lastOkAt = null;
  }
}

export const httpHealth = new HttpHealthTracker();

export type ErrorCode = components["schemas"]["ErrorCode"];
export type ErrorScope = components["schemas"]["ErrorScope"];
export type Retry = components["schemas"]["Retry"];
export type ErrorDetails = components["schemas"]["ErrorDetails"];
export type ErrorEnvelope = components["schemas"]["ErrorEnvelope"];

export class ApiError extends Error {
  readonly status: number;
  readonly code: ErrorCode;
  readonly scope: ErrorScope;
  readonly retry: Retry;
  readonly correlationId: string;
  readonly details?: ErrorDetails;
  readonly headers?: Headers;

  constructor(
    status: number,
    code: ErrorCode,
    message: string,
    scope: ErrorScope,
    retry: Retry,
    correlationId: string,
    details?: ErrorDetails,
    headers?: Headers,
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.scope = scope;
    this.retry = retry;
    this.correlationId = correlationId;
    this.details = details;
    this.headers = headers;
  }
}

export class NetworkError extends Error {
  constructor(
    message = "Сетевая ошибка при выполнении запроса",
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "NetworkError";
  }
}

export class AuthRequired extends Error {
  readonly reason: "redirect" | "unauthenticated" | "forbidden_user";

  constructor(reason: "redirect" | "unauthenticated" | "forbidden_user") {
    super(
      reason === "forbidden_user"
        ? "Доступ для этого пользователя запрещён"
        : "Сессия истекла, войдите снова",
    );
    this.name = "AuthRequired";
    this.reason = reason;
  }
}

/** Прокси ответил своей страницей ошибки или хаб не поднялся: повтор, а не вход. */
export const HUB_UNAVAILABLE = "hub_unavailable";

const AUTH_PAGE = /authelia|sign[\s-]?in|log[\s-]?in|вход|password|пароль/i;

/**
 * HTML на адрес API: страница входа или ошибка прокси. 5xx никогда не вход,
 * 2xx и 3xx всегда вход, прочие 4xx решают заголовки и разметка страницы.
 */
export function htmlVerdict(
  status: number,
  headers: Headers | null | undefined,
  body = "",
): "auth" | "unavailable" {
  if (status >= 500) return "unavailable";
  if (status < 400 || status === 401 || status === 403 || status === 407)
    return "auth";
  if (headers?.has("www-authenticate")) return "auth";
  for (const [name] of headers?.entries() ?? [])
    if (name.toLowerCase().startsWith("x-authelia")) return "auth";
  return AUTH_PAGE.test(body) ? "auth" : "unavailable";
}

/**
 * Ответ не от хаба: переадресация на вход, страница входа, ошибка прокси или отказ личности.
 * Бросает AuthRequired (и помечает сессию истёкшей) либо NetworkError(hub_unavailable).
 */
export async function screenForeign(
  response: Response,
  path: string,
): Promise<void> {
  const status = response.status;
  if (
    (response as Response & { type?: string }).type === "opaqueredirect" ||
    (status >= 300 && status < 400 && status !== 304)
  ) {
    session.state = "expired";
    throw new AuthRequired("redirect");
  }
  const contentType = response.headers?.get("Content-Type") ?? "";
  if (path.includes("/api/") && contentType.includes("text/html")) {
    const needsBody = status >= 400 && status < 500 && status !== 401 && status !== 403;
    const body = needsBody
      ? await response
          .clone()
          .text()
          .catch(() => "")
      : "";
    if (htmlVerdict(status, response.headers, body) === "unavailable")
      throw new NetworkError(HUB_UNAVAILABLE);
    session.state = "expired";
    throw new AuthRequired(status === 401 ? "unauthenticated" : "redirect");
  }
  if (status === 401) {
    session.state = "expired";
    throw new AuthRequired("unauthenticated");
  }
  if (status === 403) {
    let errorCode: string | undefined;
    try {
      errorCode = (await response.clone().json())?.error?.code;
    } catch {
      errorCode = undefined;
    }
    // origin_rejected повторный вход не исправит: он уходит обычной ошибкой с конвертом.
    if (errorCode === "forbidden_user") {
      session.state = "expired";
      throw new AuthRequired("forbidden_user");
    }
  }
}

/** Ошибка без конверта хаба: 5xx значит прокси без хаба за ним. */
export function envelopeMissing(status: number): NetworkError {
  return new NetworkError(
    status >= 500 ? HUB_UNAVAILABLE : "response_without_envelope",
  );
}

/**
 * Итог проверки входа: истёкшей сессия считается только по явному отказу,
 * сетевая ошибка или ответ без личности не должны останавливать переподключение.
 */
export function authProbeResult(error: unknown): "expired" | "down" {
  return error instanceof AuthRequired ? "expired" : "down";
}

class SessionManager {
  state: "ok" | "expired" = "ok";

  reset(): void {
    this.state = "ok";
  }

  serverNow(): number {
    return clock.serverNow();
  }
}

export const session = new SessionManager();

export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD";

export interface HttpOptions {
  idempotencyKey?: string;
  correlationId?: string;
  headers?: Record<string, string> | Headers;
  signal?: AbortSignal;
  timeoutMs?: number;
}

export type RequestOptions = HttpOptions;

export interface HttpResponse<T = unknown> {
  status: number;
  headers: Headers;
  data: T;
}

const RETRIES = new Set<Retry>([
  "safe_same_key",
  "explicit_new_operation",
  "after_condition",
  "never",
]);
const SCOPES = new Set<ErrorScope>([
  "browser",
  "auth",
  "hub_db",
  "delivery",
  "worker_disk",
  "cli_start",
  "adapter_protocol",
  "agent_service",
]);

const CONTENT_PATCH = /^\/api\/v1\/attachments\/[^/]+\/content$/;
const DRAFT_PUT = /^\/api\/v1\/conversations\/[^/]+\/draft$/;
const MARK_READ = /^\/api\/v1\/conversations\/[^/]+\/read$/;
const LIMITS_REFRESH = /^\/api\/v1\/limits\/refresh$/;
// Предпросмотр маршрута только читает: хаб принимает его без ключа идемпотентности.
const ROUTING_PREVIEW = /^\/api\/v1\/routing-policy\/preview$/;
// Поиск по истории читает, а не меняет: POST только прячет запрос из адреса.
const HISTORY_SEARCH = /^\/api\/v1\/search$/;

export function requestPathname(path: string): string {
  const bare = path.split("#")[0]?.split("?")[0] ?? path;
  if (bare.startsWith("/")) return bare;
  try {
    return new URL(bare).pathname;
  } catch {
    return bare;
  }
}

function isMutationExempt(method: HttpMethod, path: string): boolean {
  const pathname = requestPathname(path);
  if (method === "PATCH" && CONTENT_PATCH.test(pathname)) return true;
  if (method === "POST" && MARK_READ.test(pathname)) return true;
  if (method === "POST" && LIMITS_REFRESH.test(pathname)) return true;
  if (method === "POST" && ROUTING_PREVIEW.test(pathname)) return true;
  if (method === "POST" && HISTORY_SEARCH.test(pathname)) return true;
  // Полная замена черновика идемпотентна: повтор того же тела даёт тот же текст.
  if (method === "PUT" && DRAFT_PUT.test(pathname)) return true;
  return false;
}

export function envelopeOf(value: unknown): ErrorEnvelope["error"] | undefined {
  if (!value || typeof value !== "object") return undefined;
  const error = (value as { error?: unknown }).error;
  if (!error || typeof error !== "object") return undefined;
  const body = error as Record<string, unknown>;
  // Новый код или область от хаба новее вкладки всё равно ошибка хаба, а не обрыв связи.
  if (typeof body.code !== "string" || body.code.length === 0) return undefined;
  if (typeof body.message !== "string") return undefined;
  if (typeof body.scope !== "string") return undefined;
  if (typeof body.retry !== "string") return undefined;
  if (
    typeof body.correlation_id !== "string" ||
    body.correlation_id.length === 0
  )
    return undefined;
  return {
    ...(error as ErrorEnvelope["error"]),
    scope: SCOPES.has(body.scope as ErrorScope)
      ? (body.scope as ErrorScope)
      : "browser",
    retry: RETRIES.has(body.retry as Retry) ? (body.retry as Retry) : "never",
  };
}

export async function http<T = unknown>(
  method: HttpMethod,
  path: string,
  body?: unknown,
  options?: HttpOptions,
): Promise<HttpResponse<T>> {
  const isMutation = ["POST", "PUT", "PATCH", "DELETE"].includes(method);
  if (
    isMutation &&
    !isMutationExempt(method, path) &&
    !options?.idempotencyKey
  ) {
    throw new Error(
      `Мутирующий запрос ${method} ${path} требует idempotencyKey`,
    );
  }

  const correlationId = options?.correlationId ?? crypto.randomUUID();
  const headers = new Headers(options?.headers);
  headers.set("X-Canterlot-Client", "1");
  headers.set("X-Correlation-Id", correlationId);

  if (options?.idempotencyKey) {
    headers.set("Idempotency-Key", options.idempotencyKey);
  }

  let requestBody: BodyInit | null | undefined = null;
  if (body !== undefined && body !== null) {
    if (
      body instanceof Uint8Array ||
      body instanceof Blob ||
      body instanceof FormData ||
      typeof body === "string"
    ) {
      requestBody = body as BodyInit;
    } else {
      if (!headers.has("Content-Type")) {
        headers.set("Content-Type", "application/json");
      }
      requestBody = JSON.stringify(body);
    }
  }

  const contentPatch =
    method === "PATCH" && CONTENT_PATCH.test(requestPathname(path));
  const defaultTimeout = contentPatch
    ? undefined
    : isMutation
      ? 30_000
      : 15_000;
  const timeoutMs = options?.timeoutMs ?? defaultTimeout;
  const timeoutController = new AbortController();
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  if (timeoutMs !== undefined) {
    timeoutId = setTimeout(() => timeoutController.abort(), timeoutMs);
  }
  const signal =
    options?.signal && timeoutMs !== undefined
      ? AbortSignal.any([options.signal, timeoutController.signal])
      : (options?.signal ??
        (timeoutMs !== undefined ? timeoutController.signal : undefined));

  const sentAt = Date.now();
  let response: Response;

  try {
    response = await fetch(path, {
      method,
      headers,
      body: requestBody,
      credentials: "same-origin",
      redirect: "manual",
      signal,
    });
  } catch (err) {
    if (timeoutId) clearTimeout(timeoutId);
    if (err instanceof Error && err.name === "AbortError") {
      if (options?.signal?.aborted) throw err;
      throw new NetworkError("timeout", { cause: err });
    }
    throw new NetworkError(
      err instanceof Error ? err.message : "Network request failed",
      {
        cause: err,
      },
    );
  }

  const receivedAt = Date.now();

  const consume = async <R>(work: () => Promise<R>): Promise<R> => {
    return await new Promise<R>((resolve, reject) => {
      const fail = (err: unknown) => {
        if (options?.signal?.aborted) {
          reject(
            options.signal.reason instanceof Error
              ? options.signal.reason
              : err,
          );
          return;
        }
        if (timeoutController.signal.aborted) {
          reject(
            new NetworkError("timeout", {
              cause: err instanceof Error ? err : undefined,
            }),
          );
          return;
        }
        if (
          err instanceof NetworkError ||
          err instanceof ApiError ||
          err instanceof AuthRequired
        ) {
          reject(err);
          return;
        }
        reject(
          new NetworkError("invalid_response", {
            cause: err instanceof Error ? err : undefined,
          }),
        );
      };
      if (timeoutMs !== undefined) {
        if (timeoutController.signal.aborted) {
          fail(new NetworkError("timeout"));
          return;
        }
        timeoutController.signal.addEventListener(
          "abort",
          () => fail(new NetworkError("timeout")),
          { once: true },
        );
      }
      options?.signal?.addEventListener(
        "abort",
        () => fail(options.signal?.reason),
        { once: true },
      );
      work().then(resolve, fail);
    });
  };

  try {
    const serverTimeHeader = response.headers.get("X-Server-Time-Ms");
    if (serverTimeHeader) {
      const serverTimeMs = Number(serverTimeHeader);
      if (!isNaN(serverTimeMs)) {
        clock.update(serverTimeMs, sentAt, receivedAt);
      }
    }

    await consume(() => screenForeign(response, path));

    const contentType = response.headers.get("Content-Type") ?? "";

    if (!response.ok) {
      let errorBody: ErrorEnvelope["error"] | undefined;
      try {
        errorBody = envelopeOf(await consume(() => response.json()));
      } catch (err) {
        if (err instanceof NetworkError && err.message === "timeout") throw err;
        errorBody = undefined;
      }

      if (!errorBody) throw envelopeMissing(response.status);

      throw new ApiError(
        response.status,
        errorBody.code,
        errorBody.message,
        errorBody.scope,
        errorBody.retry,
        errorBody.correlation_id,
        errorBody.details,
        response.headers,
      );
    }

    httpHealth.recordSuccess(receivedAt);
    session.state = "ok";

    if (
      response.status === 204 ||
      response.headers.get("Content-Length") === "0"
    ) {
      return {
        status: response.status,
        headers: response.headers,
        data: undefined as T,
      };
    }

    if (contentType.includes("application/json")) {
      const data = (await consume(() => response.json())) as T;
      return {
        status: response.status,
        headers: response.headers,
        data,
      };
    }

    if (
      contentType.includes("application/octet-stream") ||
      contentType.startsWith("image/")
    ) {
      const data = (await consume(() => response.arrayBuffer())) as T;
      return {
        status: response.status,
        headers: response.headers,
        data,
      };
    }

    return {
      status: response.status,
      headers: response.headers,
      data: (await consume(() => response.text())) as unknown as T,
    };
  } finally {
    if (timeoutId) clearTimeout(timeoutId);
  }
}
