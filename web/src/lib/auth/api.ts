import { http } from "#lib/api/http.js";
import type { components } from "#lib/api/generated/openapi.js";

type Schemas = components["schemas"];
export type MethodsView = Schemas["MethodsView"];
export type OwnerSession = Schemas["OwnerSessionView"];
export type OwnerSessions = Schemas["OwnerSessionsView"];
export type SessionLocation = Schemas["SessionLocationView"];
export type Passkey = Schemas["PasskeyView"];
export type QrState = Schemas["QrState"];
export type QrStart = Schemas["QrStartView"];
export type QrPoll = Schemas["QrPollView"];
export type QrRequest = Schemas["QrRequestView"];
export type Challenge = { challenge_id: string; options: { publicKey: Record<string, unknown> } };
export type Device = Record<string, unknown>;
type SignIn = Schemas["SignInView"];
type Revoked = { revoked: string[] };

const base = "/api/v1/auth";
const data = async <T>(request: Promise<{ data: T }>): Promise<T> => (await request).data;

export const authApi = {
  methods: () => data(http<MethodsView>("GET", `${base}/methods`)),
  password: (login: string, password: string, device?: Device) =>
    data(http<SignIn>("POST", `${base}/password`, { login, password, device })),
  recovery: (token: string, device?: Device) =>
    data(http<SignIn>("POST", `${base}/recovery`, { token, device })),
  passkeyStart: () => data(http<Challenge>("POST", `${base}/passkey/start`, {})),
  passkeyFinish: (challenge_id: string, credential: unknown, device?: Device) =>
    data(http<SignIn>("POST", `${base}/passkey/finish`, { challenge_id, credential, device })),
  qrStart: (device?: Device) => data(http<QrStart>("POST", `${base}/qr/start`, { device })),
  // Опрос ждёт на хабе до 25 с: свой срок запроса длиннее.
  qrPoll: (poll_token: string, after: QrState, signal?: AbortSignal) =>
    data(http<QrPoll>("POST", `${base}/qr/poll`, { poll_token, after }, { signal, timeoutMs: 40_000 })),
  qrInspect: (secret: string) => data(http<QrRequest>("POST", `${base}/qr/inspect`, { secret })),
  qrApprove: (secret: string) => data(http<{ state: QrState }>("POST", `${base}/qr/approve`, { secret })),
  qrDeny: (secret: string) => data(http<{ state: QrState }>("POST", `${base}/qr/deny`, { secret })),
  sessions: () => data(http<OwnerSessions>("GET", `${base}/sessions`)),
  renameSession: (id: string, name: string) =>
    data(http<OwnerSession>("PATCH", `${base}/sessions/${encodeURIComponent(id)}`, { name })),
  deleteSession: (id: string) => data(http<Revoked>("DELETE", `${base}/sessions/${encodeURIComponent(id)}`)),
  revokeOthers: () => data(http<Revoked>("POST", `${base}/sessions/revoke-others`, {})),
  logout: () => data(http<Revoked>("POST", `${base}/logout`, {})),
  settings: (idle_timeout_days: number) =>
    data(http<{ idle_timeout_days: number }>("PATCH", `${base}/settings`, { idle_timeout_days })),
  passkeys: () => data(http<Schemas["PasskeysView"]>("GET", `${base}/passkeys`)),
  registerStart: () => data(http<Challenge>("POST", `${base}/passkeys/register/start`, {})),
  registerFinish: (challenge_id: string, credential: unknown, name?: string) =>
    data(http<Passkey>("POST", `${base}/passkeys/register/finish`, { challenge_id, credential, name })),
  renamePasskey: (id: string, name: string) =>
    data(http<Passkey>("PATCH", `${base}/passkeys/${encodeURIComponent(id)}`, { name })),
  deletePasskey: (id: string) => data(http<unknown>("DELETE", `${base}/passkeys/${encodeURIComponent(id)}`)),
  setPassword: (new_password: string, current_password?: string) =>
    data(http<Revoked>("PUT", `${base}/password`, { new_password, current_password })),
  reauthPassword: (password: string) =>
    data(http<{ fresh_until: string }>("POST", `${base}/reauth/password`, { password })),
  reauthPasskeyStart: () => data(http<Challenge>("POST", `${base}/reauth/passkey/start`, {})),
  reauthPasskeyFinish: (challenge_id: string, credential: unknown) =>
    data(http<{ fresh_until: string }>("POST", `${base}/reauth/passkey/finish`, { challenge_id, credential })),
};
