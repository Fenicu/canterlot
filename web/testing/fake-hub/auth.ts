import type { Page, Route } from '@playwright/test';
import type { components } from '../../src/lib/api/generated/openapi';

type Schemas = components['schemas'];
export type FakeSession = Schemas['OwnerSessionView'];
export type FakePasskey = Schemas['PasskeyView'];
type QrState = Schemas['QrState'];

export interface AuthCall { method: string; path: string; body: unknown }

const minutesAgo = (n: number) => new Date(Date.now() - n * 60_000).toISOString();
const id = (n: number) => `00000000-0000-4000-9000-${n.toString(16).padStart(12, '0')}`;

/** Вход владельца для UI-тестов: сеансы, ключи, пароль, QR и подтверждение входа. */
export class FakeAuth {
  login = 'owner';
  password: string | null = 'верный-пароль-1';
  signedIn = true;
  /** Ответ 401 на запросы с сеансом, пока владелец не вошёл. */
  signOut: { code: string; reason?: string } = { code: 'unauthenticated' };
  /** Сбой `GET /me` без ответа хаба о сеансе: 503 базы или обрыв сети. */
  meFailure: 503 | 'network' | null = null;
  fresh = false;
  idle = 90;
  lockSeconds = 0;
  passkeyAvailable = true;
  calls: AuthCall[] = [];
  /** Ответы опроса QR по очереди; когда очередь пуста, опрос через 300 мс возвращает то же состояние. */
  qrScript: QrState[] = [];
  qrStarts = 0;
  /** Одноразовые отказы: «METHOD /api/v1/auth/...» → статус и код. */
  failures = new Map<string, { status: number; code: string; details?: Record<string, unknown> }>();
  inspect: Schemas['QrRequestView'] = {
    created_at: minutesAgo(0), expires_at: new Date(Date.now() + 50_000).toISOString(), age_seconds: 12,
    ip: '203.0.113.9', location: { city: 'Казань', country: 'Россия', country_code: 'RU', local: false },
    label: 'Firefox 159 · Linux', other_country: false
  };
  sessions: FakeSession[] = [
    { id: id(1), name: null, label: 'Chrome 142 · Android 16 · Pixel 9', user_agent: 'Mozilla/5.0 (Linux; Android 16; Pixel 9) Chrome/142.0', method: 'password', created_at: minutesAgo(60 * 24 * 3), last_seen_at: minutesAgo(0), created_ip: '198.51.100.7', last_ip: '198.51.100.7', location: { city: 'Москва', country: 'Россия', country_code: 'RU', local: false }, approved_by: null, approved: [id(3)], current: true },
    { id: id(2), name: 'Рабочий ноутбук', label: 'Firefox 159 · Linux', user_agent: 'Mozilla/5.0 (X11; Linux x86_64; rv:159.0) Gecko/20100101 Firefox/159.0', method: 'passkey', created_at: minutesAgo(60 * 24 * 20), last_seen_at: minutesAgo(5), created_ip: '192.168.1.20', last_ip: '192.168.1.20', location: { local: true }, approved_by: null, approved: [], current: false },
    { id: id(3), name: null, label: 'Safari 19 · iOS 19', user_agent: 'Mozilla/5.0 (iPad; CPU OS 19_0 like Mac OS X) Safari/605.1.15', method: 'qr', created_at: minutesAgo(60 * 5), last_seen_at: minutesAgo(60 * 3), created_ip: null, last_ip: null, location: null, approved_by: id(1), approved: [id(4)], current: false },
    { id: id(4), name: 'Планшет на кухне', label: 'Chrome 142 · Android 15', user_agent: 'Mozilla/5.0 (Linux; Android 15) Chrome/142.0', method: 'qr', created_at: minutesAgo(60), last_seen_at: minutesAgo(30), created_ip: '203.0.113.40', last_ip: '203.0.113.40', location: { city: 'Казань', country: 'Россия', country_code: 'RU', local: false }, approved_by: id(3), approved: [], current: false }
  ];
  passkeys: FakePasskey[] = [
    { id: id(20), name: 'Bitwarden', created_at: minutesAgo(60 * 24 * 10), last_used_at: minutesAgo(60 * 24), backed_up: true }
  ];
  private next = 100;

  get current(): FakeSession { return this.sessions.find((session) => session.current)!; }

  /** Живые сеансы, подтверждённые этим, дальше по цепочке. */
  cascade(from: string): string[] {
    const found: string[] = [];
    const queue = [...(this.sessions.find((session) => session.id === from)?.approved ?? [])];
    while (queue.length) {
      const next = queue.shift()!;
      if (found.includes(next)) continue;
      found.push(next);
      queue.push(...(this.sessions.find((session) => session.id === next)?.approved ?? []));
    }
    return found;
  }

  revoke(ids: string[]): void {
    this.sessions = this.sessions.filter((session) => !ids.includes(session.id));
    for (const session of this.sessions) session.approved = session.approved.filter((item) => !ids.includes(item));
  }

  signIn(method: FakeSession['method']): FakeSession {
    this.signedIn = true;
    const session: FakeSession = { ...this.current, id: id(this.next++), method, created_at: new Date().toISOString(), approved: [], approved_by: null };
    return session;
  }

  /** Ответ 401 для запросов с сеансом, когда владелец не вошёл. */
  async refuse(route: Route): Promise<void> {
    await route.fulfill({ status: 401, json: envelope(this.signOut.code, this.signOut.reason ? { reason: this.signOut.reason } : undefined) });
  }
}

function envelope(code: string, details?: Record<string, unknown>) {
  return { error: { code, message: code, scope: 'auth', retry: 'never', correlation_id: '00000000-0000-4000-8000-0000000000a1', ...(details ? { details } : {}) } };
}

const challenge = (origin: string) => ({
  challenge_id: '00000000-0000-4000-9000-0000000000c1',
  options: { publicKey: { rp: { name: 'canterlot', id: new URL(origin).hostname }, user: { id: 'AQID', name: 'owner', displayName: 'owner' }, challenge: 'AQIDBAUGBwgJCgsMDQ4PEA', pubKeyCredParams: [{ type: 'public-key', alg: -7 }], timeout: 60_000, allowCredentials: [], excludeCredentials: [] } }
});

export async function installAuthRoutes(page: Page, auth: FakeAuth): Promise<void> {
  await page.route('**/api/v1/auth/**', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname.replace('/api/v1/auth', '');
    const method = request.method();
    let body: Record<string, unknown> = {};
    try { body = request.postDataJSON() ?? {}; } catch { body = {}; }
    auth.calls.push({ method, path, body });
    const json = (value: unknown, status = 200) => route.fulfill({ status, json: value, headers: { 'X-Server-Time-Ms': String(Date.now()) } });
    const fail = (status: number, code: string, details?: Record<string, unknown>) => route.fulfill({ status, json: envelope(code, details) });
    const failure = auth.failures.get(`${method} ${path}`);
    if (failure) { auth.failures.delete(`${method} ${path}`); return fail(failure.status, failure.code, failure.details); }
    const publicPath = ['/methods', '/password', '/recovery', '/passkey/start', '/passkey/finish', '/qr/start', '/qr/poll'].includes(path) && (method !== 'PUT');
    if (!publicPath && !auth.signedIn) return auth.refuse(route);
    const needFresh = async (work: () => Promise<void>) => { if (!auth.fresh) return fail(403, 'reauth_required'); return work(); };

    if (method === 'GET' && path === '/methods') return json({ password: auth.password !== null, passkey: auth.passkeyAvailable && auth.passkeys.length > 0 });
    if (method === 'POST' && path === '/password') {
      if (auth.lockSeconds) return fail(429, 'too_many_attempts', { retry_after_seconds: auth.lockSeconds });
      if (body.login !== auth.login || auth.password === null || body.password !== auth.password) return fail(401, 'invalid_credentials');
      return json({ session: auth.signIn('password') });
    }
    if (method === 'POST' && path === '/recovery') {
      if (body.token !== 'R'.repeat(43)) return fail(401, 'invalid_credentials');
      return json({ session: auth.signIn('recovery') });
    }
    if (method === 'POST' && (path === '/passkey/start' || path === '/reauth/passkey/start' || path === '/passkeys/register/start')) {
      if (path === '/passkeys/register/start') return needFresh(() => json(challenge(url.origin)));
      return json(challenge(url.origin));
    }
    if (method === 'POST' && path === '/passkey/finish') return json({ session: auth.signIn('passkey') });
    if (method === 'POST' && path === '/qr/start') {
      auth.qrStarts++;
      return json({ url: `${url.origin}/qr#${String(auth.qrStarts).padStart(43, 'Q')}`, poll_token: `poll-${auth.qrStarts}`, expires_at: new Date(Date.now() + 60_000).toISOString() });
    }
    if (method === 'POST' && path === '/qr/poll') {
      const next = auth.qrScript.shift();
      if (!next) { await new Promise((resolve) => setTimeout(resolve, 300)); return json({ state: body.after ?? 'pending' }).catch(() => undefined); }
      if (next === 'approved') return json({ state: 'approved', session: auth.signIn('qr') });
      return json({ state: next });
    }
    if (method === 'POST' && path === '/qr/inspect') return json(auth.inspect);
    if (method === 'POST' && path === '/qr/approve') return json({ state: 'approved' });
    if (method === 'POST' && path === '/qr/deny') return json({ state: 'denied' });
    if (method === 'GET' && path === '/sessions') return json({ current_id: auth.current.id, idle_timeout_days: auth.idle, fresh_until: auth.fresh ? new Date(Date.now() + 600_000).toISOString() : null, sessions: auth.sessions });
    const sessionMatch = /^\/sessions\/([^/]+)$/.exec(path);
    if (sessionMatch && method === 'PATCH') {
      const target = auth.sessions.find((session) => session.id === sessionMatch[1]);
      if (!target) return fail(404, 'not_found');
      target.name = String(body.name ?? '').trim() || null;
      return json(target);
    }
    if (sessionMatch && method === 'DELETE') {
      const target = sessionMatch[1]!;
      if (target === auth.current.id) { auth.signedIn = false; return json({ revoked: [target] }); }
      return needFresh(async () => { const revoked = [target, ...auth.cascade(target)]; auth.revoke(revoked); await json({ revoked }); });
    }
    if (method === 'POST' && path === '/sessions/revoke-others') return needFresh(async () => { const revoked = auth.sessions.filter((session) => !session.current).map((session) => session.id); auth.revoke(revoked); await json({ revoked }); });
    if (method === 'POST' && path === '/logout') { auth.signedIn = false; return json({ revoked: [auth.current.id] }); }
    if (method === 'PATCH' && path === '/settings') { auth.idle = Number(body.idle_timeout_days); return json({ idle_timeout_days: auth.idle }); }
    if (method === 'GET' && path === '/passkeys') return json({ passkeys: auth.passkeys });
    if (method === 'POST' && path === '/passkeys/register/finish') {
      const key: FakePasskey = { id: id(auth.passkeys.length + 50), name: String(body.name ?? 'Chrome 142 · Android 16'), created_at: new Date().toISOString(), last_used_at: null, backed_up: false };
      auth.passkeys.push(key);
      return json(key);
    }
    const keyMatch = /^\/passkeys\/([^/]+)$/.exec(path);
    if (keyMatch && method === 'PATCH') {
      const key = auth.passkeys.find((item) => item.id === keyMatch[1]);
      if (!key) return fail(404, 'not_found');
      key.name = String(body.name);
      return json(key);
    }
    if (keyMatch && method === 'DELETE') return needFresh(async () => { auth.passkeys = auth.passkeys.filter((item) => item.id !== keyMatch[1]); await json({}); });
    if (method === 'PUT' && path === '/password') {
      const change = async () => {
        auth.password = String(body.new_password);
        const revoked = auth.sessions.filter((session) => !session.current).map((session) => session.id);
        auth.revoke(revoked);
        auth.fresh = true;
        await json({ revoked });
      };
      if (auth.password !== null) return body.current_password === auth.password ? change() : fail(403, 'wrong_password');
      return needFresh(change);
    }
    if (method === 'POST' && path === '/reauth/password') {
      if (body.password !== auth.password) return fail(403, 'wrong_password');
      auth.fresh = true;
      return json({ fresh_until: new Date(Date.now() + 600_000).toISOString() });
    }
    if (method === 'POST' && path === '/reauth/passkey/finish') { auth.fresh = true; return json({ fresh_until: new Date(Date.now() + 600_000).toISOString() }); }
    return fail(404, 'not_found');
  });
}
