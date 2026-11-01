import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  http,
  ApiError,
  NetworkError,
  AuthRequired,
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

  it("redirect_or_html_on_api_is_auth_required_not_a_json_error", async () => {
    // 1. opaqueredirect
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        return Response.error(); // or mock opaqueredirect response
      }),
    );
    // We can simulate opaqueredirect response with type = 'opaqueredirect'
    const opaqueResp = new Response(null, { status: 200 });
    Object.defineProperty(opaqueResp, "type", { value: "opaqueredirect" });
    Object.defineProperty(opaqueResp, "status", { value: 0 });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => opaqueResp),
    );

    await expect(http("GET", "/api/v1/me")).rejects.toThrow(AuthRequired);
    expect(session.state).toBe("expired");

    // 2. 401 unauthenticated
    session.reset?.();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        return new Response(
          JSON.stringify({
            error: {
              code: "unauthenticated",
              message: "Требуется вход",
              scope: "auth",
              retry: "after_condition",
              correlation_id: "corr-auth-1",
            },
          }),
          {
            status: 401,
            headers: { "Content-Type": "application/json" },
          },
        );
      }),
    );
    await expect(http("GET", "/api/v1/me")).rejects.toThrow(AuthRequired);
    expect(session.state).toBe("expired");

    // 3. 403 forbidden_user
    session.reset?.();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        return new Response(
          JSON.stringify({
            error: {
              code: "forbidden_user",
              message: "Доступ запрещён",
              scope: "auth",
              retry: "never",
              correlation_id: "corr-auth-2",
            },
          }),
          {
            status: 403,
            headers: { "Content-Type": "application/json" },
          },
        );
      }),
    );
    await expect(http("GET", "/api/v1/me")).rejects.toThrow(AuthRequired);
    expect(session.state).toBe("expired");

    // 4. 403 origin_rejected
    session.reset?.();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        return new Response(
          JSON.stringify({
            error: {
              code: "origin_rejected",
              message: "Недопустимый источник",
              scope: "auth",
              retry: "never",
              correlation_id: "corr-auth-3",
            },
          }),
          {
            status: 403,
            headers: { "Content-Type": "application/json" },
          },
        );
      }),
    );
    await expect(http("GET", "/api/v1/me")).rejects.toThrow(AuthRequired);
    expect(session.state).toBe("expired");

    // 5. HTML body on /api/
    session.reset?.();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        return new Response(
          "<!DOCTYPE html><html><body>Login page</body></html>",
          {
            status: 200,
            headers: { "Content-Type": "text/html; charset=utf-8" },
          },
        );
      }),
    );
    await expect(http("GET", "/api/v1/conversations")).rejects.toThrow(
      AuthRequired,
    );
    expect(session.state).toBe("expired");
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
