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

export interface SendGateEnv {
  onLine(): boolean;
  now(): number;
  probe(): Promise<"ok" | "down">;
}

/**
 * Проверка, можно ли отправлять. Смотрит на navigator.onLine и последний успешный HTTP,
 * не на живость WebSocket.
 */
export async function canSendNow(env?: SendGateEnv): Promise<"ok" | "offline"> {
  const onLine =
    env?.onLine ??
    (() => typeof navigator === "undefined" || navigator.onLine !== false);
  const now = env?.now ?? (() => Date.now());

  if (!onLine()) return "offline";

  const last = httpHealth.lastOkAt;
  if (last !== null && now() - last <= 30_000) return "ok";

  if (env?.probe) {
    try {
      const result = await Promise.race([
        env.probe(),
        new Promise<"down">((resolve) =>
          setTimeout(() => resolve("down"), 2000),
        ),
      ]);
      if (result === "ok") {
        httpHealth.recordSuccess(now());
        return "ok";
      }
      return "offline";
    } catch {
      return "offline";
    }
  }

  try {
    const { http: request } = await import("#lib/api/http.js");
    await request("GET", "/api/v1/me", undefined, { timeoutMs: 2000 });
    return "ok";
  } catch {
    return "offline";
  }
}
