/**
 * Ключи доступа: параметры хаба приходят JSON с двоичными полями в base64url.
 * Браузеры с parse*OptionsFromJSON и toJSON разбирают их сами, для остальных — ручное кодирование.
 */
type Json = Record<string, unknown>;
type Pkc = typeof PublicKeyCredential & {
  parseCreationOptionsFromJSON?: (json: Json) => PublicKeyCredentialCreationOptions;
  parseRequestOptionsFromJSON?: (json: Json) => PublicKeyCredentialRequestOptions;
};

export function webauthnAvailable(): boolean {
  return (
    typeof window !== "undefined" &&
    window.isSecureContext &&
    typeof PublicKeyCredential !== "undefined" &&
    typeof navigator.credentials?.create === "function"
  );
}

export function fromBase64url(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

export function toBase64url(buffer: ArrayBuffer | ArrayBufferView): string {
  const bytes = buffer instanceof ArrayBuffer ? new Uint8Array(buffer) : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const descriptors = (list: unknown): PublicKeyCredentialDescriptor[] | undefined =>
  Array.isArray(list)
    ? list.map((item: Json) => ({ ...item, id: fromBase64url(String(item.id)) }) as PublicKeyCredentialDescriptor)
    : undefined;

export function creationOptions(json: Json): PublicKeyCredentialCreationOptions {
  const parse = (PublicKeyCredential as Pkc).parseCreationOptionsFromJSON;
  if (parse) {
    try {
      return parse(json);
    } catch {
      // Строгий разбор браузера споткнулся о поле расширения: ниже тот же разбор вручную.
    }
  }
  const user = json.user as Json;
  return {
    ...json,
    challenge: fromBase64url(String(json.challenge)),
    user: { ...user, id: fromBase64url(String(user.id)) },
    excludeCredentials: descriptors(json.excludeCredentials),
  } as unknown as PublicKeyCredentialCreationOptions;
}

export function requestOptions(json: Json): PublicKeyCredentialRequestOptions {
  const parse = (PublicKeyCredential as Pkc).parseRequestOptionsFromJSON;
  if (parse) {
    try {
      return parse(json);
    } catch {
      // Как и при создании: ручной разбор.
    }
  }
  return {
    ...json,
    challenge: fromBase64url(String(json.challenge)),
    allowCredentials: descriptors(json.allowCredentials),
  } as unknown as PublicKeyCredentialRequestOptions;
}

export function credentialJson(credential: PublicKeyCredential): Json {
  const own = (credential as PublicKeyCredential & { toJSON?: () => Json }).toJSON;
  if (typeof own === "function") {
    try {
      return own.call(credential);
    } catch {
      // Старый браузер без сериализации: ниже вручную.
    }
  }
  const response = credential.response as AuthenticatorResponse & Partial<AuthenticatorAttestationResponse & AuthenticatorAssertionResponse>;
  const body: Json = { clientDataJSON: toBase64url(response.clientDataJSON) };
  if (response.attestationObject) {
    body.attestationObject = toBase64url(response.attestationObject);
    body.transports = response.getTransports?.() ?? [];
  }
  if (response.authenticatorData) body.authenticatorData = toBase64url(response.authenticatorData);
  if (response.signature) body.signature = toBase64url(response.signature);
  if (response.userHandle) body.userHandle = toBase64url(response.userHandle);
  return {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    response: body,
    authenticatorAttachment: credential.authenticatorAttachment ?? undefined,
    clientExtensionResults: credential.getClientExtensionResults?.() ?? {},
  };
}

/** Отмена владельцем и отказ браузера: текст для формы. */
export function webauthnErrorText(error: unknown): string {
  const name = error instanceof Error ? error.name : "";
  if (name === "NotAllowedError" || name === "AbortError") return "Вход ключом доступа отменён или время вышло";
  if (name === "InvalidStateError") return "Этот ключ доступа уже добавлен";
  if (name === "SecurityError") return "Браузер не разрешает ключи доступа для этого адреса";
  if (name === "NotSupportedError") return "Устройство не поддерживает ключи доступа";
  return "Не удалось использовать ключ доступа";
}

export async function createCredential(options: { publicKey: Json }): Promise<Json> {
  const credential = (await navigator.credentials.create({ publicKey: creationOptions(options.publicKey) })) as PublicKeyCredential | null;
  if (!credential) throw new DOMException("cancelled", "NotAllowedError");
  return credentialJson(credential);
}

export async function getCredential(options: { publicKey: Json }): Promise<Json> {
  const credential = (await navigator.credentials.get({ publicKey: requestOptions(options.publicKey) })) as PublicKeyCredential | null;
  if (!credential) throw new DOMException("cancelled", "NotAllowedError");
  return credentialJson(credential);
}
