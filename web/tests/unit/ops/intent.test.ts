import { describe, expect, it } from "vitest";
import { ApiError, NetworkError } from "#lib/api/http.js";
import { Intents } from "#lib/ops/intent.js";
import { cancelInput, editInput } from "#lib/timeline/QueueActions.js";
import { freshInput } from "#lib/stores/inputs.js";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

const refusal = (reason: string) =>
  new ApiError(409, "invalid_state", "Действие недопустимо", "delivery", "never", "c", {
    reason: reason as never,
  });

describe("ops/intent", () => {
  it("double_tap_while_in_flight_sends_one_request", async () => {
    const intents = new Intents();
    const gate = deferred<string>();
    const keys: string[] = [];
    const call = (key: string) => {
      keys.push(key);
      return gate.promise;
    };
    const first = intents.run("stop:c", call);
    const second = intents.run("stop:c", call);
    expect(intents.busy("stop:c")).toBe(true);
    gate.resolve("ok");
    await expect(Promise.all([first, second])).resolves.toEqual(["ok", "ok"]);
    expect(keys).toHaveLength(1);
    expect(intents.busy("stop:c")).toBe(false);
  });

  it("retry_after_a_network_error_reuses_the_key_until_settled", async () => {
    const intents = new Intents();
    const keys: string[] = [];
    await intents
      .run("resume:c", async (key) => {
        keys.push(key);
        throw new NetworkError("timeout");
      })
      .catch(() => undefined);
    await intents.run("resume:c", async (key) => {
      keys.push(key);
    });
    await intents.run("resume:c", async (key) => {
      keys.push(key);
    });
    expect(keys[0]).toBe(keys[1]);
    expect(keys[2]).not.toBe(keys[1]);
  });

  it("a_refusal_releases_the_key", async () => {
    const intents = new Intents();
    const keys: string[] = [];
    await intents
      .run("group:x", async (key) => {
        keys.push(key);
        throw refusal("input_not_queued");
      })
      .catch(() => undefined);
    await intents.run("group:x", async (key) => {
      keys.push(key);
    });
    expect(keys[0]).not.toBe(keys[1]);
  });
});

describe("queue actions", () => {
  it("failed_cancel_does_not_offer_to_interrupt_the_turn", async () => {
    const input = freshInput("k1", "текст", 0);
    input.id = "i1";
    const offline = await cancelInput(input, {
      clientCancel: async () => {
        throw new NetworkError("Failed to fetch");
      },
    });
    expect(offline).toEqual({ failed: "Нет связи, отмена не отправлена" });
    const late = await cancelInput(input, { clientCancel: async () => ({ too_late: true }) });
    expect(late).toBe("lost");
  });

  it("edit_retried_after_a_timeout_reuses_its_key", async () => {
    const input = freshInput("k2", "старый", 0);
    input.id = "i2";
    const keys: string[] = [];
    let calls = 0;
    const clientEditInput = async (_c: string, _i: string, _b: unknown, key: string) => {
      keys.push(key);
      calls += 1;
      if (calls === 1) throw new NetworkError("timeout");
      return { input_id: "i2", revision: 2 };
    };
    expect(await editInput(input, "c", "новый", [], 1, { clientEditInput })).toEqual({ reason: "network" });
    expect(await editInput(input, "c", "новый", [], 1, { clientEditInput })).toBe("saved");
    expect(keys[0]).toBe(keys[1]);
  });

  it("own_edit_landed_before_the_lost_answer_is_not_a_conflict", async () => {
    const input = freshInput("k3", "новый", 0);
    input.id = "i3";
    input.revision = 2;
    const outcome = await editInput(input, "c", "новый", [], 1, {
      clientEditInput: async () => {
        throw refusal("revision_conflict");
      },
    });
    expect(outcome).toBe("saved");
    const other = freshInput("k4", "чужой", 0);
    other.id = "i4";
    other.revision = 2;
    const conflict = await editInput(other, "c", "мой", [], 1, {
      clientEditInput: async () => {
        throw refusal("revision_conflict");
      },
    });
    expect(conflict).toEqual({ reason: "revision_conflict" });
  });
});
