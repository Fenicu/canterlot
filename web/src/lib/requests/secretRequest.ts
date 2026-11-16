import { fmt } from '#lib/format/ru.js';

/** Предел значения на клиенте: 16 КиБ в UTF-8, как у хаба. */
export const SECRET_REQUEST_VALUE_LIMIT = 16 * 1024;

export type SecretRequestState =
  | 'open'
  | 'sending'
  | 'delivered'
  | 'declined'
  | 'revoking'
  | 'revoked'
  | 'expired'
  | 'lost'
  | 'cancelled';

/**
 * Ответ хаба на список, ответ и отзыв. Значения секрета здесь нет и быть не может:
 * оно живёт только в поле карточки и в теле одного запроса.
 */
export interface SecretRequestView {
  id: string;
  conversation_id: string;
  name: string;
  purpose: string;
  state: SecretRequestState;
  path?: string | null;
  expires_at?: string | null;
  note?: string | null;
  created_at: string;
  updated_at: string;
}

export type SecretRequestAnswer = { value: string } | { decline: true };

export class SecretRequestError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'SecretRequestError';
    this.status = status;
    this.code = code;
  }
}

export function isSecretValueTooLarge(value: string): boolean {
  return new TextEncoder().encode(value).length > SECRET_REQUEST_VALUE_LIMIT;
}

export function secretRequestTitle(): string {
  return 'Агент просит секрет';
}

/** Строка состояния под именем и назначением. У открытого запроса её заменяет заголовок. */
export function secretRequestLine(view: SecretRequestView): string {
  switch (view.state) {
    case 'open':
      return '';
    case 'sending':
      return 'Передаю на машину…';
    case 'delivered':
      return view.expires_at
        ? `Передан · файл до ${fmt.clock(Date.parse(view.expires_at))}`
        : 'Передан';
    case 'declined':
      return 'Отказано';
    case 'revoking':
      return 'Отзываю…';
    case 'revoked':
      return 'Отозван';
    case 'expired':
      return 'Истёк';
    case 'lost':
      return 'Значение не дошло до машины, введите снова';
    case 'cancelled':
      return 'Отменён';
  }
}

/** Поле ввода нужно только там, где значение можно ввести или повторить. */
export function secretRequestShowsField(view: SecretRequestView): boolean {
  return view.state === 'open' || view.state === 'lost';
}

/** Открытый и потерянный запрос ждут владельца у поля ввода; остальные живут в ленте. */
export function isSecretRequestDocked(view: SecretRequestView): boolean {
  return secretRequestShowsField(view);
}

export function secretRequestCanRevoke(view: SecretRequestView): boolean {
  return view.state === 'delivered';
}

export function secretRequestErrorMessage(error: unknown): string {
  if (error instanceof SecretRequestError) {
    switch (error.code) {
      case 'machine_offline':
        return 'Машина не на связи, попробуйте позже';
      case 'not_open':
        return 'Запрос уже закрыт';
      case 'not_delivered':
        return 'Значение ещё не доставлено';
      case 'limit':
        return 'Слишком часто, попробуйте позже';
      case 'too_large':
        return 'Слишком большое значение';
      case 'not_found':
        return 'Запрос не найден';
    }
    if (error.status === 413) return 'Слишком большое значение';
    return error.message || 'Не удалось передать секрет';
  }
  return 'Не удалось передать секрет';
}

function envelopeCode(body: unknown): string {
  if (!body || typeof body !== 'object') return '';
  const record = body as Record<string, unknown>;
  if (typeof record.code === 'string') return record.code;
  const error = record.error;
  if (error && typeof error === 'object' && typeof (error as Record<string, unknown>).code === 'string') {
    return String((error as Record<string, unknown>).code);
  }
  return '';
}

function envelopeMessage(body: unknown): string {
  if (!body || typeof body !== 'object') return '';
  const record = body as Record<string, unknown>;
  if (typeof record.message === 'string') return record.message;
  const error = record.error;
  if (error && typeof error === 'object' && typeof (error as Record<string, unknown>).message === 'string') {
    return String((error as Record<string, unknown>).message);
  }
  return '';
}

type Fetcher = typeof fetch;

async function requestJson<T>(url: string, init: RequestInit, fetcher: Fetcher): Promise<T> {
  let response: Response;
  try {
    response = await fetcher(url, {
      credentials: 'same-origin',
      ...init,
      headers: {
        'X-Canterlot-Client': '1',
        ...(init.headers as Record<string, string> | undefined)
      }
    });
  } catch (error) {
    throw new SecretRequestError(0, 'network', error instanceof Error ? error.message : 'Нет связи');
  }
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    throw new SecretRequestError(
      response.status,
      envelopeCode(body) || (response.status === 413 ? 'too_large' : 'request_failed'),
      envelopeMessage(body)
    );
  }
  return body as T;
}

export async function listSecretRequests(
  conversationId: string,
  fetcher: Fetcher = fetch
): Promise<SecretRequestView[]> {
  const body = await requestJson<{ items?: SecretRequestView[] }>(
    `/api/v1/conversations/${encodeURIComponent(conversationId)}/secret-requests`,
    { method: 'GET' },
    fetcher
  );
  return Array.isArray(body.items) ? body.items : [];
}

export async function answerSecretRequest(
  id: string,
  body: SecretRequestAnswer,
  fetcher: Fetcher = fetch
): Promise<SecretRequestView> {
  return requestJson<SecretRequestView>(
    `/api/v1/secret-requests/${encodeURIComponent(id)}/answer`,
    {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Idempotency-Key': crypto.randomUUID()
      },
      body: JSON.stringify(body)
    },
    fetcher
  );
}

export async function revokeSecretRequest(
  id: string,
  fetcher: Fetcher = fetch
): Promise<SecretRequestView> {
  return requestJson<SecretRequestView>(
    `/api/v1/secret-requests/${encodeURIComponent(id)}/revoke`,
    {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Idempotency-Key': crypto.randomUUID()
      }
    },
    fetcher
  );
}
