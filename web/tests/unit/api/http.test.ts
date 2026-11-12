import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  http,
  ApiError,
  NetworkError,
  AuthRequired,
  authProbeResult,
  HUB_UNAVAILABLE,
  signOutCause,
  session,
} from "#lib/api/http.js";
import { clock } from "#lib/clock.js";

describe("api/http", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    session.reset?.();
    clock.reset?.();
  });

  it("request_carries_client_header_idempotency_key_and_correlation_id", async () => {
    let capturedUrl = "";
    let capturedInit: RequestInit | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
        capturedUrl = String(url);
        capturedInit = init;
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: {
            "Content-Type": "application/json",
            "X-Server-Time-Ms": "1000",
          },
        });
      }),
    );

    const key = "test-idemp-123";
    const res = await http(
      "POST",
      "/api/v1/conversations",
      { title: "Test" },
      { idempotencyKey: key },
    );
    expect(res.status).toBe(200);
    expect(capturedUrl).toContain("/api/v1/conversations");

    const headers = new Headers(capturedInit?.headers);
    expect(headers.get("X-Canterlot-Client")).toBe("1");
    expect(headers.get("Idempotency-Key")).toBe(key);
    const correlationId = headers.get("X-Correlation-Id");
    expect(correlationId).toBeTruthy();
    expect(correlationId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i,
    );
    expect(capturedInit?.credentials).toBe("same-origin");
    expect(capturedInit?.redirect).toBe("manual");
  });

  it("mutation_without_idempotency_key_throws_before_sending", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    // POST without key must throw
    await expect(
      http("POST", "/api/v1/conversations", { title: "No Key" }),
    ).rejects.toThrow();
    // PUT without key must throw
    await expect(
      http("PUT", "/api/v1/conversations/123/crunch", { enabled: true }),
    ).rejects.toThrow();
    // PATCH without key must throw
    await expect(
      http("PATCH", "/api/v1/conversations/123", { title: "New" }),
    ).rejects.toThrow();
    // DELETE without key must throw
    await expect(http("DELETE", "/api/v1/scheduled/123")).rejects.toThrow();

    expect(fetchMock).not.toHaveBeenCalled();

    // Exempt route 1: PATCH .../content
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(
      http(
        "PATCH",
        "/api/v1/attachments/att-1/content",
        new Uint8Array([1, 2]),
      ),
    ).resolves.toBeDefined();

    // Exempt route 2: POST .../read
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(
      http("POST", "/api/v1/conversations/c-1/read", { seq: "10" }),
    ).resolves.toBeDefined();
  });

  it("error_envelope_maps_to_api_error_with_scope_retry_and_correlation_id", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        return new Response(
          JSON.stringify({
            error: {
              code: "invalid_request",
              message: "Некорректный запрос",
              scope: "browser",
              retry: "never",
              correlation_id: "corr-xyz",
            },
          }),
          {
            status: 400,
            headers: { "Content-Type": "application/json" },
          },
        );
      }),
    );

    try {
      await http("GET", "/api/v1/conversations/c-1");
      expect.unreachable("Should have thrown ApiError");
    } catch (err) {
      expect(err).toBeInstanceOf(ApiError);
      const apiErr = err as ApiError;
      expect(apiErr.status).toBe(400);
      expect(apiErr.code).toBe("invalid_request");
      expect(apiErr.scope).toBe("browser");
      expect(apiErr.retry).toBe("never");
      expect(apiErr.correlationId).toBe("corr-xyz");
      expect(apiErr.message).toBe("Некорректный запрос");
    }
  });

  const envelope401 = (code: string, reason?: string) =>
    new Response(
      JSON.stringify({
        error: {
          code,
          message: "Требуется вход",
          scope: "auth",
          retry: "after_condition",
          correlation_id: "corr-auth-1",
          ...(reason ? { details: { reason } } : {}),
        },
      }),
      { status: 401, headers: { "Content-Type": "application/json" } },
    );

  it("unauthorized_is_auth_required_with_the_hub_code_and_revoke_reason", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => envelope401("unauthenticated")));
    const plain = await http("GET", "/api/v1/me").catch((e: unknown) => e);
    expect(plain).toBeInstanceOf(AuthRequired);
    expect((plain as AuthRequired).reason).toBe("unauthenticated");
    expect(session.state).toBe("expired");
    expect(session.cause).toEqual({ code: "unauthenticated", reason: undefined, wipe: false });

    session.reset();
    vi.stubGlobal("fetch", vi.fn(async () => envelope401("session_revoked", "owner")));
    const revoked = await http("GET", "/api/v1/me").catch((e: unknown) => e);
    expect((revoked as AuthRequired).revokeReason).toBe("owner");
    expect(session.cause?.wipe).toBe(true);

    // Мягкий отказ следующего запроса не отменяет начатое стирание.
    vi.stubGlobal("fetch", vi.fn(async () => envelope401("unauthenticated")));
    await http("GET", "/api/v1/me").catch(() => undefined);
    expect(session.cause?.wipe).toBe(true);
  });

  it("revoke_reasons_decide_whether_the_device_is_wiped", () => {
    for (const reason of ["owner", "others", "server", "password_changed", "cascade"])
      expect(signOutCause("session_revoked", reason).wipe, reason).toBe(true);
    for (const reason of ["limit", "idle", "logout"])
      expect(signOutCause("session_revoked", reason).wipe, reason).toBe(false);
    expect(signOutCause("session_expired").wipe).toBe(false);
    expect(signOutCause("unauthenticated").wipe).toBe(false);
  });

  it("wrong_credentials_on_sign_in_stay_a_form_error", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => envelope401("invalid_credentials")));
    const failure = await http("POST", "/api/v1/auth/password", { login: "owner", password: "x" }).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).code).toBe("invalid_credentials");
    expect(session.state).toBe("ok");
    expect(session.cause).toBeNull();
  });

  it("wrong_password_while_signed_in_is_not_a_sign_out", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: { code: "wrong_password", message: "Неверный пароль", scope: "auth", retry: "never", correlation_id: "c" } }), { status: 403, headers: { "Content-Type": "application/json" } })),
    );
    const failure = await http("POST", "/api/v1/auth/reauth/password", { password: "x" }).catch((e: unknown) => e);
    expect((failure as ApiError).code).toBe("wrong_password");
    expect(session.cause).toBeNull();
  });

  it("success_after_sign_out_restores_but_public_sign_in_paths_do_not", async () => {
    const seen: unknown[] = [];
    const stop = session.subscribe((cause) => seen.push(cause));
    session.expire("unauthenticated");
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ password: true, passkey: false }), { status: 200, headers: { "Content-Type": "application/json" } })));
    await http("GET", "/api/v1/auth/methods");
    expect(session.cause).not.toBeNull();
    await http("GET", "/api/v1/me");
    expect(session.cause).toBeNull();
    expect(seen).toEqual([{ code: "unauthenticated", reason: undefined, wipe: false }, null]);
    stop();
  });

  it("redirect_or_html_on_api_is_hub_unavailable_not_a_sign_in", async () => {
    const opaqueResp = new Response(null, { status: 200 });
    Object.defineProperty(opaqueResp, "type", { value: "opaqueredirect" });
    Object.defineProperty(opaqueResp, "status", { value: 0 });
    vi.stubGlobal("fetch", vi.fn(async () => opaqueResp));
    const redirected = await http("GET", "/api/v1/me").catch((e: unknown) => e);
    expect((redirected as NetworkError).message).toBe(HUB_UNAVAILABLE);

    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("<!DOCTYPE html><html><body>Login page</body></html>", { status: 200, headers: { "Content-Type": "text/html; charset=utf-8" } })),
    );
    const html = await http("GET", "/api/v1/conversations").catch((e: unknown) => e);
    expect((html as NetworkError).message).toBe(HUB_UNAVAILABLE);
    expect(session.state).toBe("ok");

    // Адрес вне allowed_origins повторный вход не исправит: это ошибка настройки.
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: { code: "origin_rejected", message: "Недопустимый источник", scope: "auth", retry: "never", correlation_id: "corr-auth-3" } }), { status: 403, headers: { "Content-Type": "application/json" } })),
    );
    const rejected = await http("GET", "/api/v1/me").catch((e: unknown) => e);
    expect((rejected as ApiError).code).toBe("origin_rejected");
    expect(session.state).toBe("ok");
  });

  it("proxy_error_page_is_hub_unavailable_not_an_expired_login", async () => {
    for (const status of [502, 503, 504]) {
      session.reset();
      vi.stubGlobal(
        "fetch",
        vi.fn(
          async () =>
            new Response("<html><body><h1>502 Bad Gateway</h1>nginx</body></html>", {
              status,
              headers: { "Content-Type": "text/html" },
            }),
        ),
      );
      const failure = await http("GET", "/api/v1/conversations").catch((e: unknown) => e);
      expect(failure).toBeInstanceOf(NetworkError);
      expect((failure as NetworkError).message).toBe(HUB_UNAVAILABLE);
      expect(session.state).toBe("ok");
    }
  });

  it("plain_text_5xx_without_envelope_is_hub_unavailable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("Bad Gateway", { status: 502, headers: { "Content-Type": "text/plain" } })),
    );
    const failure = await http("GET", "/api/v1/conversations").catch((e: unknown) => e);
    expect((failure as NetworkError).message).toBe(HUB_UNAVAILABLE);
  });

  it("manual_redirect_status_is_hub_unavailable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(null, { status: 302, headers: { Location: "https://auth.example/" } })),
    );
    const failure = await http("GET", "/api/v1/me").catch((e: unknown) => e);
    expect((failure as NetworkError).message).toBe(HUB_UNAVAILABLE);
    expect(session.state).toBe("ok");
  });

  it("unknown_error_code_from_a_newer_hub_is_an_api_error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              error: { code: "brand_new", message: "Новая ошибка", scope: "future", retry: "later", correlation_id: "c-new" },
            }),
            { status: 409, headers: { "Content-Type": "application/json" } },
          ),
      ),
    );
    const failure = await http("GET", "/api/v1/me").catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).retry).toBe("never");
    expect((failure as ApiError).message).toBe("Новая ошибка");
  });

  it("network_failure_on_a_mutation_is_outcome_unknown_not_a_refusal", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("Failed to fetch");
      }),
    );

    await expect(
      http(
        "POST",
        "/api/v1/conversations",
        { title: "New" },
        { idempotencyKey: "key-1" },
      ),
    ).rejects.toBeInstanceOf(NetworkError);
  });

  it("clock_offset_comes_from_x_server_time_ms_with_the_smallest_round_trip", async () => {
    let now = 10000;
    vi.spyOn(Date, "now").mockImplementation(() => now);

    // First request: sentAt = 10000, receivedAt = 10200, rtt = 200, serverMs = 15100
    // midpoint = 10100, offset = 15100 - 10100 = 5000
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        now += 200;
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: {
            "Content-Type": "application/json",
            "X-Server-Time-Ms": "15100",
          },
        });
      }),
    );

    await http("GET", "/api/v1/me");
    expect(clock.offset).toBe(5000);

    // Second request: larger rtt (400), should NOT override
    // sentAt = 11000, receivedAt = 11400, rtt = 400, serverMs = 20000
    now = 11000;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        now += 400;
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: {
            "Content-Type": "application/json",
            "X-Server-Time-Ms": "20000",
          },
        });
      }),
    );

    await http("GET", "/api/v1/me");
    expect(clock.offset).toBe(5000);

    // Third request: smaller rtt (100), SHOULD override
    // sentAt = 12000, receivedAt = 12100, rtt = 100, serverMs = 18050
    // midpoint = 12050, offset = 18050 - 12050 = 6000
    now = 12000;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        now += 100;
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: {
            "Content-Type": "application/json",
            "X-Server-Time-Ms": "18050",
          },
        });
      }),
    );

    await http("GET", "/api/v1/me");
    expect(clock.offset).toBe(6000);
  });

  it("idempotency_exemption_matches_only_content_and_read_pathnames", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      http("PATCH", "/api/v1/conversations/x/settings?hint=/content", {
        title: "x",
      }),
    ).rejects.toThrow(/idempotencyKey/);
    await expect(
      http("POST", "/api/v1/conversations/x/already-read", { seq: "1" }),
    ).rejects.toThrow(/idempotencyKey/);
    expect(fetchMock).not.toHaveBeenCalled();

    await http(
      "PATCH",
      "/api/v1/attachments/att-1/content?part=1",
      new Uint8Array([1]),
    );
    await http("POST", "/api/v1/conversations/c-1/read?seq=10", { seq: "10" });
    await http("POST", "/api/v1/limits/refresh");
    expect(fetchMock).toHaveBeenCalledTimes(3);
    await expect(http("POST", "/api/v1/limits/refresh-all")).rejects.toThrow(
      /idempotencyKey/,
    );
  });

  it("malformed_error_envelope_is_an_unknown_outcome", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ error: { code: "internal" } }), {
            status: 502,
            headers: { "Content-Type": "application/json" },
          }),
      ),
    );

    await expect(
      http(
        "POST",
        "/api/v1/conversations",
        { title: "x" },
        { idempotencyKey: "k" },
      ),
    ).rejects.toBeInstanceOf(NetworkError);
  });

  it("deadline_covers_the_response_body_and_broken_json_is_unknown", async () => {
    const stalled = new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
    Object.defineProperty(stalled, "json", {
      value: () => new Promise(() => undefined),
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => stalled),
    );

    const pending = http("GET", "/api/v1/me", undefined, { timeoutMs: 5 });
    const settled = await Promise.race([
      pending.then(
        () => "resolved",
        () => "rejected",
      ),
      new Promise((resolve) => setTimeout(() => resolve("pending"), 30)),
    ]);
    expect(settled).toBe("rejected");
    await expect(pending).rejects.toBeInstanceOf(NetworkError);

    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response('{"ok":', {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
      ),
    );
    await expect(http("GET", "/api/v1/me")).rejects.toBeInstanceOf(
      NetworkError,
    );
  });

  it("successful_me_clears_expiry", async () => {
    session.state = "expired";
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ owner_id: "alice" }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
      ),
    );
    await http("GET", "/api/v1/me");
    expect(session.state).toBe("ok");
  });

  it("auth_probe_only_treats_auth_requirements_as_an_expired_session", () => {
    expect(authProbeResult(new AuthRequired("unauthenticated"))).toBe("expired");
    expect(authProbeResult(new NetworkError("timeout"))).toBe("down");
    expect(authProbeResult(new Error("boom"))).toBe("down");
  });

  it("read_times_out_at_15s_and_mutation_at_30s_and_abort_is_unknown", async () => {
    vi.useFakeTimers();
    try {
      vi.stubGlobal(
        "fetch",
        vi.fn((_url: string, init?: RequestInit) => {
          return new Promise((_resolve, reject) => {
            const fail = () => {
              const err = new Error("The operation was aborted");
              err.name = "AbortError";
              reject(err);
            };
            if (init?.signal?.aborted) fail();
            else init?.signal?.addEventListener("abort", fail, { once: true });
          });
        }),
      );

      let readDone = false;
      const read = http("GET", "/api/v1/me").then(
        () => {
          readDone = true;
          return "ok";
        },
        (err: unknown) => {
          readDone = true;
          return err;
        },
      );
      await vi.advanceTimersByTimeAsync(14_999);
      expect(readDone).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(await read).toBeInstanceOf(NetworkError);

      let writeDone = false;
      const write = http(
        "POST",
        "/api/v1/conversations",
        { title: "a" },
        { idempotencyKey: "k" },
      ).then(
        () => {
          writeDone = true;
          return "ok";
        },
        (err: unknown) => {
          writeDone = true;
          return err;
        },
      );
      await vi.advanceTimersByTimeAsync(29_999);
      expect(writeDone).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(await write).toBeInstanceOf(NetworkError);

      const controller = new AbortController();
      const aborted = http("GET", "/api/v1/config", undefined, {
        signal: controller.signal,
      });
      controller.abort();
      await expect(aborted).rejects.toMatchObject({ name: "AbortError" });
    } finally {
      vi.useRealTimers();
    }
  });
});
