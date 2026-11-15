/** Провайдеры, чьи ошибки приходят блоками `error` от адаптеров воркера. */
export type ProviderId = 'claude' | 'codex' | 'opencode' | 'antigravity';

/**
 * `account` — вход или проверка аккаунта, `rate_limit` — 429 и упор в квоту,
 * `overloaded` — 529 и явная перегрузка, `server` — прочие 5xx, `api_other` — остальное.
 */
export type ApiErrorKind = 'account' | 'rate_limit' | 'overloaded' | 'server' | 'api_other';

/** Разобранная ошибка API провайдера: вид, статус, ссылка действия и исходный текст. */
export type ProviderApiError = {
  provider: ProviderId | null;
  kind: ApiErrorKind;
  status: number | null;
  href: string | null;
  text: string;
};

type HarnessRef = { id?: string; provider?: string } | null | undefined;

type Found = { provider: ProviderId | null; kind: ApiErrorKind; status: number | null };

/** Домены, ссылки с которых можно открыть владельцу: только сам провайдер. */
const PROVIDER_DOMAINS = ['anthropic.com', 'claude.ai', 'claude.com', 'openai.com', 'chatgpt.com'];

const PROVIDER_NAMES: [RegExp, ProviderId][] = [
  [/opencode/, 'opencode'],
  [/antigravity/, 'antigravity'],
  [/codex/, 'codex'],
  [/claude|anthropic/, 'claude']
];

/**
 * Провайдер харнесса: сначала идентификатор (`claude_code`, `synthetic-codex`),
 * потом поле провайдера — в старых снимках оно бывает подписью (`anthropic`).
 */
export function providerOf(harness: HarnessRef): ProviderId | null {
  if (!harness) return null;
  for (const value of [harness.id, harness.provider]) {
    const name = value?.trim().toLowerCase();
    if (!name) continue;
    const found = PROVIDER_NAMES.find(([pattern]) => pattern.test(name));
    if (found) return found[1];
  }
  return null;
}

function providerFromText(text: string): ProviderId | null {
  if (/Ошибка OpenCode/u.test(text)) return 'opencode';
  if (/\bCodex\b/u.test(text)) return 'codex';
  if (/\bClaude\b|Anthropic/u.test(text)) return 'claude';
  if (/\bAntigravity\b|gemini/iu.test(text)) return 'antigravity';
  return null;
}

function kindOfStatus(status: number): ApiErrorKind {
  if (status === 401 || status === 403) return 'account';
  if (status === 429) return 'rate_limit';
  if (status === 529) return 'overloaded';
  if (status >= 500) return 'server';
  return 'api_other';
}

const STATUS_LABEL = /\b(?:HTTP|status|code|API error)\s*[:/]?\s*(\d{3})\b/iu;
const STATUS_WORD = /\b(\d{3})\s+(?:Unauthorized|Forbidden|Too Many Requests|Bad Request|Internal Server Error|Service Unavailable|Overloaded|Not Found)\b/iu;

function statusOf(text: string): number | null {
  const match = STATUS_LABEL.exec(text) ?? STATUS_WORD.exec(text);
  return match ? Number(match[1]) : null;
}

const URL_TEXT = /https:\/\/[^\s<>"'`]+/giu;
const URL_TAIL = /[.,;:!?)\]}"'»”]+$/u;

/** Первая https-ссылка на домене провайдера; прочие ссылки не показываются никогда. */
function linkIn(text: string): string | null {
  for (const match of text.matchAll(URL_TEXT)) {
    const href = match[0].replace(URL_TAIL, '');
    let url: URL;
    try {
      url = new URL(href);
    } catch {
      continue;
    }
    if (url.protocol !== 'https:') continue;
    const host = url.hostname.toLowerCase();
    if (!PROVIDER_DOMAINS.some((domain) => host === domain || host.endsWith(`.${domain}`))) continue;
    return href;
  }
  return null;
}

function claudeError(text: string, provider: ProviderId | null): Found | null {
  // Строка ошибки хода Claude Code: «{status}: {error}».
  const prefixed = /^([45]\d{2}):\s*\S/u.exec(text);
  if (prefixed) {
    const status = Number(prefixed[1]);
    return { provider, kind: kindOfStatus(status), status };
  }
  if (!/^API Error:/iu.test(text)) {
    // Адаптер заменяет 404 своим текстом, а пустую ошибку — своим запасным.
    if (provider === 'claude' && /^model not found$/u.test(text))
      return { provider, kind: 'api_other', status: null };
    if (provider === 'claude' && /^api error$/iu.test(text))
      return { provider, kind: 'api_other', status: null };
    if (provider === 'claude' && /please (?:run )?\/?login|not logged in|please log in/iu.test(text))
      return { provider, kind: 'account', status: null };
    return null;
  }
  const status = statusOf(text);
  if (status !== null) return { provider, kind: kindOfStatus(status), status };
  const lower = text.toLowerCase();
  const kind: ApiErrorKind = /overload|over capacity/u.test(lower)
    ? 'overloaded'
    : /rate.?limit|too many requests/u.test(lower)
      ? 'rate_limit'
      : /verification|verify|login|log in|api key/u.test(lower)
        ? 'account'
        : 'api_other';
  return { provider, kind, status: null };
}

/** Тексты ошибок хода Codex собирает `turn_error_text` и `on_turn_error`. */
const CODEX_TEXT: [RegExp, ApiErrorKind][] = [
  [/^Сервер перегружен(?: \(HTTP (\d{3})\))?$/u, 'overloaded'],
  [/^Внутренняя ошибка сервера(?: \(HTTP (\d{3})\))?$/u, 'server'],
  [/^Некорректный запрос(?: \(HTTP (\d{3})\))?$/u, 'api_other'],
  [/^Слишком много неудачных попыток(?: \(HTTP (\d{3})\))?$/u, 'api_other'],
  [/^Ошибка соединения(?: \(HTTP (\d{3})\))?$/u, 'api_other'],
  [/^Codex требует входа/u, 'account'],
  [/^Исчерпан лимит Codex$/u, 'rate_limit'],
  [/^Codex ограничил частоту запросов$/u, 'rate_limit'],
  [/^Модель не найдена$/u, 'api_other']
];

function codexError(text: string, provider: ProviderId | null): Found | null {
  for (const [pattern, kind] of CODEX_TEXT) {
    const match = pattern.exec(text);
    if (!match) continue;
    return { provider, kind, status: match[1] ? Number(match[1]) : null };
  }
  return null;
}

/** OpenCode называет тип отказа провайдера в скобках: `provider.auth`, `provider.quota`. */
const OPENCODE_TEXT = /^Ошибка OpenCode \((provider\.[\w-]+)\):\s?([\s\S]*)$/u;

const OPENCODE_KIND: Record<string, ApiErrorKind> = {
  'provider.auth': 'account',
  'provider.rate-limit': 'rate_limit',
  'provider.overloaded': 'overloaded',
  'provider.quota': 'rate_limit',
  'provider.free-limit': 'rate_limit',
  'provider.go-limit': 'rate_limit',
  'provider.chatgpt-limit': 'rate_limit',
  'provider.budget-limit': 'rate_limit',
  'provider.internal': 'server',
  'provider.invalid-request': 'api_other',
  'provider.model-budget-limit': 'api_other'
};

function opencodeError(text: string, provider: ProviderId | null): Found | null {
  const match = OPENCODE_TEXT.exec(text);
  if (!match) return null;
  // Без типа провайдер не доказан: это может быть отказ самого сервера OpenCode.
  if (!match[1]?.startsWith('provider.')) return null;
  const kind = OPENCODE_KIND[match[1]] ?? 'api_other';
  return { provider, kind, status: statusOf(match[2] ?? '') };
}

/** Antigravity отдаёт текст отказа JSON-RPC как есть: только явные признаки провайдера. */
function antigravityError(text: string, provider: ProviderId | null): Found | null {
  const status = statusOf(text);
  if (status !== null) return { provider, kind: kindOfStatus(status), status };
  const lower = text.toLowerCase();
  if (/quota\s+(?:reached|exceeded|exhausted)|resource_exhausted/u.test(lower))
    return { provider, kind: 'rate_limit', status: null };
  if (/rate.?limit|too many requests/u.test(lower))
    return { provider, kind: 'rate_limit', status: null };
  if (/overload|over capacity/u.test(lower)) return { provider, kind: 'overloaded', status: null };
  if (/unauthorized|forbidden|invalid api key|authentication|not logged in|log ?in/u.test(lower))
    return { provider, kind: 'account', status: null };
  if (/internal server error|server error/u.test(lower))
    return { provider, kind: 'server', status: null };
  if (/api error/u.test(lower)) return { provider, kind: 'api_other', status: null };
  return null;
}

/** Без известного харнесса остаются только безошибочные признаки ошибки API. */
function genericError(text: string): Found | null {
  const prefixed = /^([45]\d{2}):\s*API Error:/iu.exec(text);
  if (prefixed) {
    const status = Number(prefixed[1]);
    return { provider: null, kind: kindOfStatus(status), status };
  }
  if (/^API Error:/iu.test(text)) return { provider: null, kind: 'api_other', status: null };
  return null;
}

/**
 * Разбирает текст блока `error`. Всё, что не доказано как ошибка API провайдера
 * (отказ инструмента, ошибка адаптера или воркера), остаётся обычным уведомлением.
 */
export function providerApiError(text: string, provider: ProviderId | null = null): ProviderApiError | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const owner = provider ?? providerFromText(trimmed);
  let found: Found | null;
  switch (owner) {
    case 'claude':
      found = claudeError(trimmed, owner);
      break;
    case 'codex':
      found = codexError(trimmed, owner);
      break;
    case 'opencode':
      found = opencodeError(trimmed, owner);
      break;
    case 'antigravity':
      found = antigravityError(trimmed, owner);
      break;
    default:
      found = genericError(trimmed);
  }
  if (!found) return null;
  return { ...found, href: linkIn(trimmed), text };
}
