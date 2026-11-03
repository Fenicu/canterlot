import { beforeEach, describe, expect, it } from "vitest";
import { httpHealth } from "#lib/api/gate.js";
import {
  ApiError,
  AuthRequired,
  HUB_UNAVAILABLE,
  NetworkError,
  session,
} from "#lib/api/http.js";
import { opRequest, sendOp } from "#lib/ops/request.js";

describe("opRequest", () => {
  it("posts_an_input_with_the_same_idempotency_key", () => {
    const request = opRequest({
      kind: "input",
      key: "key-1",
      conversationId: "conv",
      body: { content: [{ type: "text", text: "привет" }] },
    });
    expect(request.url).toBe("/api/v1/conversations/conv/inputs");
    expect(new Headers(request.init.headers).get("Idempotency-Key")).toBe(
      "key-1",
    );
    expect(request.init.method).toBe("POST");
  });

  it("posts_a_new_conversation_to_the_collection", () => {
    const request = opRequest({
      kind: "create_conversation",
      key: "key-3",
      body: {
        machine_id: "m",
        workdir: "/srv",
        harness: "claude",
        model: "opus",
        effort: "low",
        start: true,
      },
    });
    expect(request.url).toBe("/api/v1/conversations");
    expect(new Headers(request.init.headers).get("Idempotency-Key")).toBe(
      "key-3",
    );
    expect(JSON.parse(String(request.init.body)).start).toBe(true);
  });

  it("posts_a_scheduled_message_to_the_scheduled_endpoint", () => {
    const request = opRequest({
      kind: "scheduled",
      key: "key-2",
      conversationId: "conv",
      body: { run_at: "2026-10-29T12:00:00.000Z" },
    });
    expect(request.url).toBe("/api/v1/conversations/conv/scheduled");
    expect(JSON.parse(String(request.init.body)).run_at).toBe(
      "2026-10-29T12:00:00.000Z",
    );
  });

  it("posts_a_decision_to_the_decision_endpoint", () => {
    const request = opRequest({
      kind: "other",
      key: "key-decision",
      conversationId: "conv-99",
      body: {
        requestId: "req-42",
        conversationId: "conv-99",
        decision: { kind: "permission", behavior: "allow" },
      },
    });
    expect(request.url).toBe(
      "/api/v1/conversations/conv-99/requests/req-42/decision",
    );
    expect(new Headers(request.init.headers).get("Idempotency-Key")).toBe(
      "key-decision",
    );
    expect(JSON.parse(String(request.init.body))).toEqual({
      kind: "permission",
      behavior: "allow",
    });
  });
});

describe("sendOp", () => {
  const op = {
    kind: "input" as const,
    key: "key-9",
    conversationId: "conv",
    body: { content: [{ type: "text", text: "привет" }] },
  };
  const reply = (status: number, body: unknown) =>
    (async () =>
      new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;

  beforeEach(() => httpHealth.reset());

  it("committed_answer_counts_as_fresh_http_for_the_send_gate", async () => {
    const before = Date.now();
    await expect(sendOp(reply(201, { input_id: "i" }), op)).resolves.toEqual({
      input_id: "i",
    });
    expect(httpHealth.lastOkAt).toBeGreaterThanOrEqual(before);
  });

  it("refusal_keeps_the_envelope_and_does_not_count_as_fresh_http", async () => {
    const envelope = {
      error: {
        code: "queue_full",
        message: "Очередь заполнена",
        scope: "delivery",
        retry: "after_condition",
        correlation_id: "c",
      },
    };
    const failure = await sendOp(reply(409, envelope), op).catch((e) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).code).toBe("queue_full");
    expect(httpHealth.lastOkAt).toBeNull();
  });

  it("does_not_follow_redirects", async () => {
    let init: RequestInit | undefined;
    const capture = (async (_url: string, given: RequestInit) => {
      init = given;
      return new Response(JSON.stringify({ input_id: "i" }), {
        status: 201,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;
    await sendOp(capture, op);
    expect(init?.redirect).toBe("manual");
  });

  it("sso_redirect_is_auth_required_and_never_committed", async () => {
    session.reset();
    const opaque = new Response(null, { status: 200 });
    Object.defineProperty(opaque, "type", { value: "opaqueredirect" });
    Object.defineProperty(opaque, "status", { value: 0 });
    const failure = await sendOp((async () => opaque) as unknown as typeof fetch, op).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(AuthRequired);
    expect(session.state).toBe("expired");
    expect(httpHealth.lastOkAt).toBeNull();
  });

  it("login_page_with_200_is_auth_required_not_success", async () => {
    session.reset();
    const page = (async () =>
      new Response("<html><title>Authelia</title></html>", {
        status: 200,
        headers: { "content-type": "text/html; charset=utf-8" },
      })) as unknown as typeof fetch;
    await expect(sendOp(page, op)).rejects.toBeInstanceOf(AuthRequired);
    expect(session.state).toBe("expired");
  });

  it("hub_401_waits_for_sign_in_instead_of_a_refusal", async () => {
    session.reset();
    const envelope = {
      error: {
        code: "unauthenticated",
        message: "Требуется вход",
        scope: "auth",
        retry: "after_condition",
        correlation_id: "c",
      },
    };
    await expect(sendOp(reply(401, envelope), op)).rejects.toBeInstanceOf(AuthRequired);
    expect(session.state).toBe("expired");
  });

  it("proxy_502_page_is_hub_unavailable_and_retryable", async () => {
    session.reset();
    const gateway = (async () =>
      new Response("<html><h1>502 Bad Gateway</h1></html>", {
        status: 502,
        headers: { "content-type": "text/html" },
      })) as unknown as typeof fetch;
    const failure = await sendOp(gateway, op).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(NetworkError);
    expect((failure as NetworkError).message).toBe(HUB_UNAVAILABLE);
    expect(session.state).toBe("ok");
  });

  it("success_without_a_json_body_is_not_committed", async () => {
    const empty = (async () => new Response(null, { status: 204 })) as unknown as typeof fetch;
    await expect(sendOp(empty, op)).rejects.toBeInstanceOf(NetworkError);
    const text = (async () =>
      new Response("ok", { status: 200, headers: { "content-type": "text/plain" } })) as unknown as typeof fetch;
    await expect(sendOp(text, op)).rejects.toBeInstanceOf(NetworkError);
    expect(httpHealth.lastOkAt).toBeNull();
  });

  it("network_failure_is_a_network_error", async () => {
    const broken = (async () => {
      throw new TypeError("Failed to fetch");
    }) as unknown as typeof fetch;
    await expect(sendOp(broken, op)).rejects.toBeInstanceOf(NetworkError);
    expect(httpHealth.lastOkAt).toBeNull();
  });

  it("hung_post_times_out_as_a_retryable_network_error", async () => {
    const fetchImpl = ((_url: string, init: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        init.signal?.addEventListener("abort", () =>
          reject(init.signal?.reason as Error),
        );
      })) as unknown as typeof fetch;

    await expect(
      sendOp(
        fetchImpl,
        { kind: "input", key: "k", conversationId: "c", body: {} },
        5,
      ),
    ).rejects.toBeInstanceOf(NetworkError);
  });

  it("response_body_cut_by_the_timeout_stays_unknown", async () => {
    const fetchImpl = ((_url: string, init: RequestInit) =>
      Promise.resolve({
        ok: true,
        json: () =>
          new Promise((_resolve, reject) => {
            init.signal?.addEventListener("abort", () =>
              reject(init.signal?.reason as Error),
            );
          }),
      })) as unknown as typeof fetch;

    await expect(
      sendOp(
        fetchImpl,
        { kind: "input", key: "k", conversationId: "c", body: {} },
        5,
      ),
    ).rejects.toBeInstanceOf(NetworkError);
  });
});
