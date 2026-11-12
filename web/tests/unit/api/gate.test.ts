import { describe, it, expect, beforeEach, vi } from "vitest";
import { canSendNow, httpHealth } from "#lib/api/gate.js";
import { session } from "#lib/api/http.js";

describe("api/gate", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    httpHealth.reset?.();
    session.reset?.();
    Object.defineProperty(navigator, "onLine", {
      value: true,
      configurable: true,
      writable: true,
    });
  });

  it("offline_gate_uses_navigator_online_and_the_http_outcome_not_wss", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    // 1. navigator.onLine === false -> returns false immediately without probing
    Object.defineProperty(navigator, "onLine", { value: false });
    expect(await canSendNow()).toBe("offline");
    expect(fetchMock).not.toHaveBeenCalled();

    // 2. navigator.onLine === true and recent successful HTTP <= 30s -> returns true without probe
    Object.defineProperty(navigator, "onLine", { value: true });
    httpHealth.recordSuccess(Date.now() - 5000); // 5s ago
    expect(await canSendNow()).toBe("ok");
    expect(fetchMock).not.toHaveBeenCalled();

    // 3. Last HTTP success was > 30s ago -> probes GET /api/v1/me
    httpHealth.recordSuccess(Date.now() - 35000); // 35s ago
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ owner_id: "u1" }), { status: 200 }),
    );
    expect(await canSendNow()).toBe("ok");
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // 4. Probe returns 401: the session expires and nothing may be sent
    httpHealth.reset?.();
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: { code: "unauthenticated" } }), {
        status: 401,
      }),
    );
    expect(await canSendNow()).toBe("offline");
    expect(session.state).toBe("expired");

    // 5. Probe returns 401 session_revoked: same authentication failure, with the reason
    session.reset?.();
    httpHealth.reset?.();
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: { code: "session_revoked", details: { reason: "owner" } } }), {
        status: 401,
      }),
    );
    expect(await canSendNow()).toBe("offline");
    expect(session.state).toBe("expired");
    expect(session.cause?.wipe).toBe(true);

    // 6. Probe throws network error -> returns false
    httpHealth.reset?.();
    fetchMock.mockReset();
    fetchMock.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    expect(await canSendNow()).toBe("offline");

    // 7. Probe times out (e.g. takes > 3000ms or AbortSignal fires) -> returns false
    httpHealth.reset?.();
    fetchMock.mockReset();
    fetchMock.mockImplementationOnce(async (_url, init) => {
      return new Promise((_, reject) => {
        init?.signal?.addEventListener("abort", () => {
          const err = new Error("The operation was aborted");
          err.name = "AbortError";
          reject(err);
        });
      });
    });
    // With fake timers or short probe
    vi.useFakeTimers();
    const probePromise = canSendNow();
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());
    await vi.advanceTimersByTimeAsync(2100);
    expect(await probePromise).toBe("offline");
    vi.useRealTimers();
  });

  it("injected_probe_down_and_the_two_second_deadline_are_offline", async () => {
    httpHealth.reset?.();
    const down = await canSendNow({
      onLine: () => true,
      now: () => Date.now(),
      probe: async () => "down",
    });
    expect(down).toBe("offline");

    vi.useFakeTimers();
    try {
      let finished = false;
      const stalled = canSendNow({
        onLine: () => true,
        now: () => Date.now(),
        probe: () => new Promise(() => undefined),
      }).then((value) => {
        finished = true;
        return value;
      });
      await vi.advanceTimersByTimeAsync(1999);
      expect(finished).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(await stalled).toBe("offline");
    } finally {
      vi.useRealTimers();
    }
  });
});
