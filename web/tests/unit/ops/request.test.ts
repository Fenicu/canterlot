import { describe, expect, it } from "vitest";
import { opRequest } from "#lib/ops/request.js";

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
});
