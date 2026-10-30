import { beforeEach, describe, expect, it } from "vitest";
import { httpHealth } from "#lib/api/gate.js";
import { ApiError, NetworkError } from "#lib/api/http.js";
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

  it("network_failure_is_a_network_error", async () => {
    const broken = (async () => {
      throw new TypeError("Failed to fetch");
    }) as unknown as typeof fetch;
    await expect(sendOp(broken, op)).rejects.toBeInstanceOf(NetworkError);
    expect(httpHealth.lastOkAt).toBeNull();
  });
});
