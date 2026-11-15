import { describe, expect, it } from 'vitest';
import { providerApiError, providerOf } from '#lib/timeline/apiError.js';

const VERIFY =
  "403: API Error: Your account can't receive replies from Claude right now. You can find the next step in the Anthropic Verification Portal: https://portal.anthropic.com/verify?org=01a1dfb1-10bc-7230-8b59-a74abd632567";

describe('timeline/apiError claude', () => {
  it('a 403 with the verification portal is an account error with the provider link', () => {
    expect(providerApiError(VERIFY, 'claude')).toMatchObject({
      provider: 'claude',
      kind: 'account',
      status: 403,
      href: 'https://portal.anthropic.com/verify?org=01a1dfb1-10bc-7230-8b59-a74abd632567',
      text: VERIFY
    });
  });

  it('a 401 asks for the login again', () => {
    expect(providerApiError('401: Please run /login', 'claude')).toMatchObject({
      provider: 'claude',
      kind: 'account',
      status: 401,
      href: null
    });
  });

  it('429 is a rate limit and 529 is overloaded', () => {
    expect(providerApiError('429: usage_limit_reached', 'claude')).toMatchObject({
      kind: 'rate_limit',
      status: 429
    });
    expect(providerApiError('529: API Error: Overloaded', 'claude')).toMatchObject({
      kind: 'overloaded',
      status: 529
    });
  });

  it('other 5xx is the provider side and 4xx is an API error', () => {
    expect(providerApiError('503: unavailable', 'claude')).toMatchObject({ kind: 'server', status: 503 });
    expect(
      providerApiError(
        '400: API Error: 400 Input prompt is too long: 200000 tokens exceeds the maximum of 200000.',
        'claude'
      )
    ).toMatchObject({ kind: 'api_other', status: 400 });
  });

  it('an API error without a status still belongs to the provider', () => {
    const text = 'API Error: Connection refused — a firewall or proxy may be blocking it (ECONNREFUSED)';
    expect(providerApiError(text, 'claude')).toMatchObject({ kind: 'api_other', status: null, text });
  });

  it('the provider is read from the harness when the text does not name it', () => {
    expect(providerApiError('401: Please run /login', null)).toBeNull();
    expect(providerApiError('403: API Error: Your account is blocked', null)).toMatchObject({
      provider: null,
      kind: 'account'
    });
    expect(providerApiError(VERIFY, null)?.provider).toBe('claude');
  });

  it('the adapter text of a missing model and its empty fallback are provider errors too', () => {
    expect(providerApiError('model not found', 'claude')).toMatchObject({
      provider: 'claude',
      kind: 'api_other',
      status: null
    });
    expect(providerApiError('API error', 'claude')).toMatchObject({ kind: 'api_other', status: null });
  });
});

describe('timeline/apiError codex', () => {
  it('an overloaded app server carries its status', () => {
    expect(providerApiError('Сервер перегружен (HTTP 503)', 'codex')).toMatchObject({
      provider: 'codex',
      kind: 'overloaded',
      status: 503
    });
  });

  it('the recorded error names are classified by meaning', () => {
    const cases: [string, string, number | null][] = [
      ['Внутренняя ошибка сервера (HTTP 500)', 'server', 500],
      ['Некорректный запрос (HTTP 400)', 'api_other', 400],
      ['Codex требует входа: выполните codex login на машине', 'account', null],
      ['Исчерпан лимит Codex', 'rate_limit', null],
      ['Codex ограничил частоту запросов', 'rate_limit', null],
      ['Слишком много неудачных попыток', 'api_other', null],
      ['Модель не найдена', 'api_other', null]
    ];
    for (const [text, kind, status] of cases) {
      expect(providerApiError(text, 'codex'), text).toMatchObject({ provider: 'codex', kind, status });
    }
  });

  it('the adapter fallback is not a provider API error', () => {
    expect(providerApiError('Ошибка Codex', 'codex')).toBeNull();
  });
});

describe('timeline/apiError opencode', () => {
  it('provider kinds from the recorded frames are classified', () => {
    const cases: [string, string][] = [
      ['Ошибка OpenCode (provider.auth): invalid api key', 'account'],
      ['Ошибка OpenCode (provider.rate-limit): slow down', 'rate_limit'],
      ['Ошибка OpenCode (provider.quota): Go usage limit reached', 'rate_limit'],
      ['Ошибка OpenCode (provider.internal): секрет провайдера', 'server'],
      [
        'Ошибка OpenCode (provider.invalid-request): Upstream request failed: Model is unavailable.',
        'api_other'
      ]
    ];
    for (const [text, kind] of cases) {
      expect(providerApiError(text, 'opencode'), text).toMatchObject({ provider: 'opencode', kind });
    }
  });

  it('an untyped failure stays a notice: the provider is not proven', () => {
    expect(
      providerApiError(
        'Ошибка OpenCode (unknown): effect/sql/SqlError/LockTimeoutError: database is locked',
        'opencode'
      )
    ).toBeNull();
  });
});

describe('timeline/apiError antigravity', () => {
  it('quota errors are rate limits', () => {
    expect(providerApiError('quota exceeded', 'antigravity')).toMatchObject({
      provider: 'antigravity',
      kind: 'rate_limit'
    });
    expect(
      providerApiError(
        'Individual quota reached for gemini-3.8-flash-high. Resets in 2h 15m',
        'antigravity'
      )
    ).toMatchObject({ provider: 'antigravity', kind: 'rate_limit' });
  });

  it('an explicit status or login message is classified', () => {
    expect(providerApiError('API error: 503 Service Unavailable', 'antigravity')).toMatchObject({
      kind: 'server',
      status: 503
    });
    expect(
      providerApiError('HTTP 403 Forbidden: the caller does not have permission', 'antigravity')
    ).toMatchObject({ kind: 'account', status: 403 });
  });

  it('adapter errors of the same harness stay notices', () => {
    expect(providerApiError('trajectory not found', 'antigravity')).toBeNull();
    expect(providerApiError('summary failed', 'antigravity')).toBeNull();
  });
});

describe('timeline/apiError non provider errors', () => {
  it('tool failures, adapter errors and owner stops are left alone', () => {
    for (const text of [
      'Запуск остановлен: делегированной сессии доступны чужие серверы MCP (x), а разрешены только canterlot.',
      'Ход не дождался ответа CLI',
      'invalid tool input',
      'boom',
      ''
    ])
      expect(providerApiError(text, 'claude'), text).toBeNull();
  });
});

describe('timeline/apiError links', () => {
  const account = (url: string) => `403: API Error: blocked. Details: ${url}`;

  it('only provider domains become the action link', () => {
    for (const url of [
      'https://portal.anthropic.com/verify?org=1',
      'https://claude.ai/settings',
      'https://claude.com/settings',
      'https://platform.openai.com/account',
      'https://chatgpt.com/help'
    ])
      expect(providerApiError(account(url), 'claude')?.href, url).toBe(url.replace(/\.$/, ''));
  });

  it('a lookalike or non-provider domain is never a link', () => {
    for (const url of [
      'https://anthropic.com.evil.example/verify',
      'https://evil.example/anthropic.com',
      'http://portal.anthropic.com/verify',
      'https://example.com/verify'
    ])
      expect(providerApiError(account(url), 'claude')?.href, url).toBeNull();
  });

  it('the first allowlisted url wins and trailing punctuation is dropped', () => {
    const text =
      '403: API Error: blocked at https://example.com/x, then https://portal.anthropic.com/verify?org=1).';
    expect(providerApiError(text, 'claude')?.href).toBe('https://portal.anthropic.com/verify?org=1');
  });
});

describe('timeline/apiError providerOf', () => {
  it('harness ids and providers of the worker map to the four providers', () => {
    expect(providerOf({ id: 'claude_code', provider: 'claude' })).toBe('claude');
    expect(providerOf({ id: 'claude', provider: 'anthropic' })).toBe('claude');
    expect(providerOf({ id: 'codex', provider: 'openai' })).toBe('codex');
    expect(providerOf({ id: 'synthetic-codex', provider: 'codex' })).toBe('codex');
    expect(providerOf({ id: 'synthetic-opencode', provider: 'opencode' })).toBe('opencode');
    expect(providerOf({ id: 'opencode', provider: '' })).toBe('opencode');
    expect(providerOf({ id: 'antigravity', provider: 'google' })).toBe('antigravity');
    expect(providerOf(null)).toBeNull();
  });
});
