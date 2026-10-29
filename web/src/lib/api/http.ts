import { clock } from '$lib/clock';
import { httpHealth } from '$lib/api/gate';
import type { components } from '$lib/api/generated/openapi';

export type ErrorCode = components['schemas']['ErrorCode'];
export type ErrorScope = components['schemas']['ErrorScope'];
export type Retry = components['schemas']['Retry'];
export type ErrorDetails = components['schemas']['ErrorDetails'];
export type ErrorEnvelope = components['schemas']['ErrorEnvelope'];

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
    headers?: Headers
  ) {
    super(message);
    this.name = 'ApiError';
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
  constructor(message = 'Сетевая ошибка при выполнении запроса', options?: ErrorOptions) {
    super(message, options);
    this.name = 'NetworkError';
  }
}

export class AuthRequired extends Error {
  readonly reason: 'redirect' | 'unauthenticated' | 'forbidden_user';

  constructor(reason: 'redirect' | 'unauthenticated' | 'forbidden_user') {
    super(`Требуется авторизация: ${reason}`);
    this.name = 'AuthRequired';
    this.reason = reason;
  }
}

class SessionManager {
  state: 'ok' | 'expired' = 'ok';

  reset(): void {
    this.state = 'ok';
  }

  serverNow(): number {
    return clock.serverNow();
  }
}

export const session = new SessionManager();

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'HEAD';

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

const RETRIES = new Set<Retry>(['safe_same_key', 'explicit_new_operation', 'after_condition', 'never']);
const SCOPES = new Set<ErrorScope>([
  'browser',
  'auth',
  'hub_db',
  'delivery',
  'worker_disk',
  'cli_start',
  'adapter_protocol',
  'agent_service'
]);
const CODES = new Set<ErrorCode>([
  'unauthenticated', 'forbidden_user', 'origin_rejected', 'invalid_request', 'invalid_offset', 'not_found',
  'method_not_allowed', 'idempotency_conflict', 'decision_conflict', 'request_not_open', 'decision_invalid',
  'resource_limit', 'not_implemented', 'too_many_requests',
  'db_unavailable', 'internal', 'queue_full', 'conversation_closed', 'execution_already_live', 'invalid_state',
  'not_pending', 'schedule_time_out_of_range', 'too_many_scheduled', 'machine_not_found', 'machine_unavailable',
  'capability_missing', 'limits_mismatch', 'harness_unknown', 'model_unknown', 'effort_not_allowed',
  'permission_mode_unknown', 'settings_midsession_unsupported', 'profile_unknown', 'workdir_not_allowed',
  'group_not_found', 'group_name_taken', 'archive_active', 'attachment_not_ready', 'attachment_not_uploadable',
  'attachment_unsupported', 'attachment_unavailable', 'offset_mismatch', 'upload_in_progress', 'checksum_mismatch',
  'attachment_conflict'
]);

const CONTENT_PATCH = /^\/api\/v1\/attachments\/[^/]+\/content$/;
const MARK_READ = /^\/api\/v1\/conversations\/[^/]+\/read$/;

export function requestPathname(path: string): string {
  const bare = path.split('#')[0]?.split('?')[0] ?? path;
  if (bare.startsWith('/')) return bare;
  try {
    return new URL(bare).pathname;
  } catch {
    return bare;
  }
}

function isMutationExempt(method: HttpMethod, path: string): boolean {
  const pathname = requestPathname(path);
  if (method === 'PATCH' && CONTENT_PATCH.test(pathname)) return true;
  if (method === 'POST' && MARK_READ.test(pathname)) return true;
  return false;
}

export function envelopeOf(value: unknown): ErrorEnvelope['error'] | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const error = (value as { error?: unknown }).error;
  if (!error || typeof error !== 'object') return undefined;
  const body = error as Record<string, unknown>;
  if (typeof body.code !== 'string' || !CODES.has(body.code as ErrorCode)) return undefined;
  if (typeof body.message !== 'string') return undefined;
  if (typeof body.scope !== 'string' || !SCOPES.has(body.scope as ErrorScope)) return undefined;
  if (typeof body.retry !== 'string' || !RETRIES.has(body.retry as Retry)) return undefined;
  if (typeof body.correlation_id !== 'string' || body.correlation_id.length === 0) return undefined;
  return error as ErrorEnvelope['error'];
}

export async function http<T = unknown>(
  method: HttpMethod,
  path: string,
  body?: unknown,
  options?: HttpOptions
): Promise<HttpResponse<T>> {
  const isMutation = ['POST', 'PUT', 'PATCH', 'DELETE'].includes(method);
  if (isMutation && !isMutationExempt(method, path) && !options?.idempotencyKey) {
    throw new Error(`Мутирующий запрос ${method} ${path} требует idempotencyKey`);
  }

  const correlationId = options?.correlationId ?? crypto.randomUUID();
  const headers = new Headers(options?.headers);
  headers.set('X-Canterlot-Client', '1');
  headers.set('X-Correlation-Id', correlationId);

  if (options?.idempotencyKey) {
    headers.set('Idempotency-Key', options.idempotencyKey);
  }

  let requestBody: BodyInit | null | undefined = null;
  if (body !== undefined && body !== null) {
    if (
      body instanceof Uint8Array ||
      body instanceof Blob ||
      body instanceof FormData ||
      typeof body === 'string'
    ) {
      requestBody = body as BodyInit;
    } else {
      if (!headers.has('Content-Type')) {
        headers.set('Content-Type', 'application/json');
      }
      requestBody = JSON.stringify(body);
    }
  }

  const contentPatch = method === 'PATCH' && CONTENT_PATCH.test(requestPathname(path));
  const defaultTimeout = contentPatch ? undefined : isMutation ? 30_000 : 15_000;
  const timeoutMs = options?.timeoutMs ?? defaultTimeout;
  const timeoutController = new AbortController();
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  if (timeoutMs !== undefined) {
    timeoutId = setTimeout(() => timeoutController.abort(), timeoutMs);
  }
  const signal =
    options?.signal && timeoutMs !== undefined
      ? AbortSignal.any([options.signal, timeoutController.signal])
      : (options?.signal ?? (timeoutMs !== undefined ? timeoutController.signal : undefined));

  const sentAt = Date.now();
  let response: Response;

  try {
    response = await fetch(path, {
      method,
      headers,
      body: requestBody,
      credentials: 'same-origin',
      redirect: 'manual',
      signal
    });
  } catch (err) {
    if (timeoutId) clearTimeout(timeoutId);
    if (err instanceof Error && err.name === 'AbortError') {
      if (options?.signal?.aborted) throw err;
      throw new NetworkError('timeout', { cause: err });
    }
    throw new NetworkError(err instanceof Error ? err.message : 'Network request failed', {
      cause: err
    });
  }

  const receivedAt = Date.now();

  const consume = async <R>(work: () => Promise<R>): Promise<R> => {
    return await new Promise<R>((resolve, reject) => {
      const fail = (err: unknown) => {
        if (options?.signal?.aborted) {
          reject(options.signal.reason instanceof Error ? options.signal.reason : err);
          return;
        }
        if (timeoutController.signal.aborted) {
          reject(new NetworkError('timeout', { cause: err instanceof Error ? err : undefined }));
          return;
        }
        if (err instanceof NetworkError || err instanceof ApiError || err instanceof AuthRequired) {
          reject(err);
          return;
        }
        reject(new NetworkError('invalid_response', { cause: err instanceof Error ? err : undefined }));
      };
      if (timeoutMs !== undefined) {
        if (timeoutController.signal.aborted) {
          fail(new NetworkError('timeout'));
          return;
        }
        timeoutController.signal.addEventListener('abort', () => fail(new NetworkError('timeout')), { once: true });
      }
      options?.signal?.addEventListener('abort', () => fail(options.signal?.reason), { once: true });
      work().then(resolve, fail);
    });
  };

  try {

  const serverTimeHeader = response.headers.get('X-Server-Time-Ms');
  if (serverTimeHeader) {
    const serverTimeMs = Number(serverTimeHeader);
    if (!isNaN(serverTimeMs)) {
      clock.update(serverTimeMs, sentAt, receivedAt);
    }
  }

  if ((response as Response & { type?: string }).type === 'opaqueredirect') {
    session.state = 'expired';
    throw new AuthRequired('redirect');
  }

  const contentType = response.headers.get('Content-Type') ?? '';

  if (path.includes('/api/') && contentType.includes('text/html')) {
    session.state = 'expired';
    throw new AuthRequired('redirect');
  }

  if (response.status === 401) {
    session.state = 'expired';
    throw new AuthRequired('unauthenticated');
  }

  if (response.status === 403) {
    let errorCode: string | undefined;
    try {
      const errJson = await response.clone().json();
      errorCode = errJson?.error?.code;
    } catch {
      // ignore JSON parse failure
    }
    if (errorCode === 'forbidden_user' || errorCode === 'origin_rejected') {
      session.state = 'expired';
      throw new AuthRequired(errorCode === 'forbidden_user' ? 'forbidden_user' : 'redirect');
    }
  }

  if (!response.ok) {
    let errorBody: ErrorEnvelope['error'] | undefined;
    try {
      errorBody = envelopeOf(await consume(() => response.json()));
    } catch (err) {
      if (err instanceof NetworkError && err.message === 'timeout') throw err;
      errorBody = undefined;
    }

    if (!errorBody) {
      throw new NetworkError('response_without_envelope');
    }

    throw new ApiError(
      response.status,
      errorBody.code,
      errorBody.message,
      errorBody.scope,
      errorBody.retry,
      errorBody.correlation_id,
      errorBody.details,
      response.headers
    );
  }

  httpHealth.recordSuccess(receivedAt);
  session.state = 'ok';

  if (response.status === 204 || response.headers.get('Content-Length') === '0') {
    return {
      status: response.status,
      headers: response.headers,
      data: undefined as T
    };
  }

  if (contentType.includes('application/json')) {
    const data = (await consume(() => response.json())) as T;
    return {
      status: response.status,
      headers: response.headers,
      data
    };
  }

  if (contentType.includes('application/octet-stream') || contentType.startsWith('image/')) {
    const data = (await consume(() => response.arrayBuffer())) as T;
    return {
      status: response.status,
      headers: response.headers,
      data
    };
  }

  return {
    status: response.status,
    headers: response.headers,
    data: (await consume(() => response.text())) as unknown as T
  };
  } finally {
    if (timeoutId) clearTimeout(timeoutId);
  }
}
