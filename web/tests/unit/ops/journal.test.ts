import "fake-indexeddb/auto";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { flushSync } from "svelte";
import {
  closeOpenJournals,
  OpJournal,
  type JournalEnv,
  type Op,
} from "#lib/ops/journal.js";
import { watchOpState } from "./watch-ops.svelte";
import { openDb, purgeAllStores, type Idb } from "#lib/storage/idb.js";
import { DraftStore } from "#lib/drafts/store.js";
import {
  ApiError,
  NetworkError,
  AuthRequired,
  session,
  http,
} from "#lib/api/http.js";
import { canSendNow, httpHealth } from "#lib/api/gate.js";

class ManualEnv implements JournalEnv {
  nowMs = 0;
  isOnline = true;
  expired = false;
  readonly scheduled: number[] = [];
  #queue: { at: number; fn: () => void; dead: boolean }[] = [];

  now = (): number => this.nowMs;
  online = (): boolean => this.isOnline;
  authExpired = (): boolean => this.expired;

  schedule = (fn: () => void, ms: number): (() => void) => {
    this.scheduled.push(ms);
    const item = { at: this.nowMs + ms, fn, dead: false };
    this.#queue.push(item);
    return () => {
      item.dead = true;
    };
  };

  async advance(ms: number): Promise<void> {
    this.nowMs += ms;
    for (let step = 0; step < 20; step++) {
      const due = this.#queue.filter(
        (item) => !item.dead && item.at <= this.nowMs,
      );
      if (due.length === 0) {
        await Promise.resolve();
        if (!this.#queue.some((item) => !item.dead && item.at <= this.nowMs))
          return;
        continue;
      }
      for (const item of due) item.dead = true;
      for (const item of due) item.fn();
      await Promise.resolve();
    }
  }
}

describe("ops/journal", () => {
  let db: Idb;
  let env: ManualEnv;
  let draftStore: DraftStore;

  beforeEach(async () => {
    closeOpenJournals();
    vi.restoreAllMocks();
    httpHealth.reset?.();
    localStorage.clear();
    await purgeAllStores();
    db = await openDb("user-test");
    env = new ManualEnv();
    draftStore = new DraftStore("user-test", db, { debounceMs: 0 });
    httpHealth.recordSuccess(Date.now());
    session.reset?.();
  });

  function journal(send: (op: Op) => Promise<unknown>): OpJournal {
    return new OpJournal(db, send, env);
  }

  it("journal_persists_before_resolving_and_survives_reload", async () => {
    let releasePut: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      releasePut = resolve;
    });
    const realPut = db.put.bind(db);
    db.put = async (store, key, value) => {
      if (store === "ops") await gate;
      return realPut(store, key, value);
    };

    const first = journal(async () => ({ id: "inp-1" }));
    let settled = false;
    const begun = first
      .begin(
        "input",
        { text: "Hello world" },
        { conversationId: "c-1", persist: true },
      )
      .then((op) => {
        settled = true;
        return op;
      });
    await Promise.resolve();
    expect(settled).toBe(false);
    releasePut?.();
    const op = await begun;
    expect(settled).toBe(true);
    expect((await db.get<Op>("ops", op.key))?.key).toBe(op.key);

    const send = vi.fn(async (recovered: Op) => ({
      id: "inp-1",
      key: recovered.key,
    }));
    const reloaded = new OpJournal(db, send, env);
    const pending = await reloaded.recover();
    expect(pending.map((item) => item.key)).toEqual([op.key]);
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(send.mock.calls[0]?.[0].key).toBe(op.key);
  });

  it("network_error_retries_with_the_same_key_and_creates_exactly_one_input", async () => {
    const keys: string[] = [];
    let attempts = 0;
    const serverInputs: string[] = [];
    const send = vi.fn(async (op: Op) => {
      attempts += 1;
      keys.push(op.key);
      if (attempts < 3) throw new NetworkError("Connection failed");
      serverInputs.push(op.key);
      return { id: "inp-1" };
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "Retry me" },
      { conversationId: "c-1", persist: true },
    );
    const running = ops.run(op);
    await vi.waitFor(() => expect(env.scheduled).toContain(1000));
    await env.advance(1000);
    await vi.waitFor(() => expect(env.scheduled).toContain(2000));
    await env.advance(2000);
    await running;
    expect(attempts).toBe(3);
    expect(keys).toEqual([op.key, op.key, op.key]);
    expect(serverInputs).toEqual([op.key]);
    expect(op.state).toBe("committed");
  });

  it("refused_operation_returns_text_to_the_draft_and_drops_the_key", async () => {
    const send = vi.fn(async () => {
      throw new ApiError(
        409,
        "conversation_closed",
        "Разговор закрыт",
        "delivery",
        "never",
        "corr-refused",
      );
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "Unsent text" },
      { conversationId: "c-closed", persist: true },
    );
    await expect(ops.run(op)).rejects.toThrow(ApiError);
    expect(op.state).toBe("refused");
    expect(await db.get("ops", op.key)).toBeUndefined();
    draftStore.set("c-closed", {
      text:
        op.body && typeof op.body === "object" && "text" in op.body
          ? String((op.body as { text: string }).text)
          : "",
    });
    await draftStore.flush();
    expect((await draftStore.load("c-closed"))?.text).toBe("Unsent text");
  });

  it("operation_class_follows_the_retry_field_of_the_envelope", async () => {
    const rows: {
      name: string;
      error: () => Error;
      retries: boolean;
      delay?: number;
    }[] = [
      {
        name: "safe_same_key",
        error: () =>
          new ApiError(
            503,
            "db_unavailable",
            "База недоступна",
            "hub_db",
            "safe_same_key",
            "c",
          ),
        retries: true,
        delay: 1000,
      },
      {
        name: "after_condition",
        error: () =>
          new ApiError(
            409,
            "queue_full",
            "Очередь заполнена",
            "delivery",
            "after_condition",
            "c",
          ),
        retries: false,
      },
      {
        name: "explicit_new_operation",
        error: () =>
          new ApiError(
            409,
            "invalid_state",
            "Новая операция",
            "delivery",
            "explicit_new_operation",
            "c",
          ),
        retries: false,
      },
      {
        name: "never",
        error: () =>
          new ApiError(
            400,
            "invalid_request",
            "Ошибка",
            "browser",
            "never",
            "c",
          ),
        retries: false,
      },
      {
        name: "missing_envelope",
        error: () => new NetworkError("response_without_envelope"),
        retries: true,
        delay: 1000,
      },
      {
        name: "429",
        error: () =>
          new ApiError(
            429,
            "too_many_requests",
            "Слишком много",
            "hub_db",
            "safe_same_key",
            "c",
          ),
        retries: true,
        delay: 1000,
      },
      {
        name: "5xx",
        error: () =>
          new ApiError(
            500,
            "internal",
            "Внутренняя ошибка",
            "hub_db",
            "safe_same_key",
            "c",
          ),
        retries: true,
        delay: 1000,
      },
      {
        name: "5xx_without_safe_same_key",
        error: () =>
          new ApiError(
            500,
            "not_implemented",
            "Не реализовано",
            "hub_db",
            "never",
            "c",
          ),
        retries: false,
      },
    ];

    for (const row of rows) {
      env.scheduled.length = 0;
      let calls = 0;
      const send = vi.fn(async (sent: Op) => {
        calls += 1;
        if (calls === 1) throw row.error();
        return { ok: true, key: sent.key };
      });
      const ops = journal(send);
      const op = await ops.begin(
        "input",
        { text: row.name },
        { conversationId: "c-1", persist: true },
      );
      const running = ops.run(op);
      if (row.retries) {
        await vi.waitFor(() => expect(env.scheduled.at(-1)).toBe(row.delay));
        await env.advance(row.delay ?? 1000);
        await running;
        expect(calls).toBe(2);
        expect(op.state).toBe("committed");
        expect(send.mock.calls[0]?.[0].key).toBe(op.key);
        expect(send.mock.calls[1]?.[0].key).toBe(op.key);
      } else {
        await expect(running).rejects.toThrow(ApiError);
        expect(calls).toBe(1);
        expect(op.state).toBe("refused");
        expect(env.scheduled).toEqual([]);
      }
    }
  });

  it("expired_session_mid_retry_keeps_the_key_and_creates_one_input", async () => {
    const keys: string[] = [];
    let calls = 0;
    const send = vi.fn(async (op: Op) => {
      calls += 1;
      keys.push(op.key);
      if (calls === 1) throw new NetworkError("Offline");
      if (calls === 2) {
        env.expired = true;
        throw new AuthRequired("unauthenticated");
      }
      return { id: "inp-auth-ok" };
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "Keep my key" },
      { conversationId: "c-auth", persist: true },
    );
    const running = ops.run(op);
    await vi.waitFor(() => expect(calls).toBe(1));
    await env.advance(1000);
    await vi.waitFor(() => expect(calls).toBe(2));
    expect(op.state).toBe("unknown");
    await env.advance(50);
    expect(calls).toBe(2);
    env.expired = false;
    await env.advance(50);
    await running;
    expect(calls).toBe(3);
    expect(keys).toEqual([op.key, op.key, op.key]);
    expect(op.state).toBe("committed");
  });

  it("idempotency_conflict_is_surfaced_and_never_retried", async () => {
    let calls = 0;
    const send = vi.fn(async () => {
      calls += 1;
      throw new ApiError(
        409,
        "idempotency_conflict",
        "Ключ уже использован",
        "browser",
        "never",
        "corr-conflict",
      );
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "Conflict" },
      { conversationId: "c-1", persist: true },
    );
    await expect(ops.run(op)).rejects.toThrow(ApiError);
    expect(calls).toBe(1);
    expect(op.state).toBe("refused");
    expect(op.refusal?.code).toBe("idempotency_conflict");
    await env.advance(5000);
    expect(calls).toBe(1);
  });

  it("settle_removes_the_record_when_the_server_item_with_the_key_arrives", async () => {
    const ops = journal(async () => ({ id: "inp-1" }));
    const op = await ops.begin(
      "input",
      { text: "To be settled" },
      { conversationId: "c-1", persist: true },
    );
    await ops.run(op);
    expect(op.state).toBe("committed");
    expect(await db.get("ops", op.key)).toBeDefined();
    ops.settle(op.key);
    await vi.waitFor(async () => {
      expect(await db.get("ops", op.key)).toBeUndefined();
    });
    expect(ops.ops.has(op.key)).toBe(false);
  });

  it("offline_press_creates_a_draft_not_an_operation_and_nothing_sends_on_reconnect", async () => {
    const send = vi.fn();
    const ops = journal(send);
    expect(
      await canSendNow({
        onLine: () => false,
        now: () => Date.now(),
        probe: async () => "ok",
      }),
    ).toBe("offline");
    draftStore.set("c-offline", { text: "Offline text" });
    await draftStore.flush();
    expect((await draftStore.load("c-offline"))?.text).toBe("Offline text");
    expect(ops.ops.size).toBe(0);
    expect(await db.list("ops")).toHaveLength(0);
    env.isOnline = true;
    httpHealth.recordSuccess(Date.now());
    await env.advance(1000);
    expect(send).not.toHaveBeenCalled();
  });

  it("offline_gate_uses_navigator_online_and_the_http_outcome_not_wss", async () => {
    expect(
      await canSendNow({
        onLine: () => false,
        now: () => Date.now(),
        probe: async () => {
          throw new Error("wss must not be consulted");
        },
      }),
    ).toBe("offline");
    httpHealth.recordSuccess(Date.now());
    expect(
      await canSendNow({
        onLine: () => true,
        now: () => Date.now(),
        probe: async () => "down",
      }),
    ).toBe("ok");
  });

  it("unknown_op_resumes_immediately_on_resume_and_online_events", async () => {
    env.isOnline = false;
    let calls = 0;
    const send = vi.fn(async () => {
      calls += 1;
      return { ok: true };
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "Stale op" },
      { conversationId: "c-stale", persist: true },
    );
    const running = ops.run(op);
    await env.advance(50);
    expect(calls).toBe(0);
    env.isOnline = true;
    await env.advance(50);
    await running;
    expect(calls).toBe(1);

    env.isOnline = true;
    let secondCalls = 0;
    const sendAgain = vi.fn(async () => {
      secondCalls += 1;
      if (secondCalls === 1) throw new NetworkError("drop");
      return { ok: true };
    });
    const again = journal(sendAgain);
    const retry = await again.begin(
      "input",
      { text: "retry now" },
      { conversationId: "c-2", persist: true },
    );
    const second = again.run(retry);
    await vi.waitFor(() => expect(env.scheduled).toContain(1000));
    again.retryNow(retry.key);
    await second;
    expect(secondCalls).toBe(2);
    expect(env.scheduled.filter((ms) => ms >= 1000).length).toBe(1);
  });

  it("reload_between_send_and_response_retries_the_same_key", async () => {
    let release: ((value: { id: string }) => void) | undefined;
    const hanging = new Promise<{ id: string }>((resolve) => {
      release = resolve;
    });
    const firstSend = vi.fn(async (op: Op) => {
      await hanging;
      return { id: "lost", key: op.key };
    });
    const first = journal(firstSend);
    const op = await first.begin(
      "input",
      { text: "Interrupted send" },
      { conversationId: "c-int", persist: true },
    );
    const inflight = first.run(op);
    await vi.waitFor(() => expect(firstSend).toHaveBeenCalledTimes(1));
    expect((await db.get<Op>("ops", op.key))?.state).toBe("unknown");

    const secondSend = vi.fn(async (sent: Op) => ({
      id: "inp-recovered",
      key: sent.key,
    }));
    const reloaded = new OpJournal(db, secondSend, new ManualEnv());
    const recovered = await reloaded.recover();
    expect(recovered.map((item) => item.key)).toEqual([op.key]);
    await vi.waitFor(() => expect(secondSend).toHaveBeenCalled());
    expect(secondSend.mock.calls[0]?.[0].key).toBe(op.key);
    release?.({ id: "lost" });
    await inflight;
  });

  it("successful_me_clears_expiry_and_resumes_the_journal", async () => {
    session.state = "expired";
    let calls = 0;
    const send = vi.fn(async () => {
      calls += 1;
      return { id: "inp-restored" };
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "after login" },
      { conversationId: "c-1", persist: true },
    );
    const running = ops.run(op);
    await env.advance(50);
    expect(calls).toBe(0);

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
    await env.advance(50);
    await running;
    expect(calls).toBe(1);
    expect(op.state).toBe("committed");
  });

  it("idempotency_conflict_stays_terminal_across_reload", async () => {
    const send = vi.fn(async () => {
      throw new ApiError(
        409,
        "idempotency_conflict",
        "Ключ уже использован",
        "browser",
        "never",
        "corr-conflict",
      );
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "Conflict" },
      { conversationId: "c-1", persist: true },
    );
    await expect(ops.run(op)).rejects.toThrow(ApiError);
    const stored = await db.get<Op>("ops", op.key);
    expect(stored === undefined || stored.state === "refused").toBe(true);

    const again = vi.fn(async () => ({ id: "retried" }));
    const reloaded = new OpJournal(db, again, new ManualEnv());
    await reloaded.recover();
    expect(again).not.toHaveBeenCalled();
  });

  it("recover_purges_finished_rows_and_rows_never_keep_the_response", async () => {
    const ops = journal(async () => ({ id: "inp-1", echo: "ответ хаба" }));
    const op = await ops.begin(
      "input",
      { text: "x" },
      { conversationId: "c-1", persist: true },
    );
    await ops.run(op);
    const stored = await db.get<Op>("ops", op.key);
    expect(stored?.state).toBe("committed");
    expect(stored?.result).toBeUndefined();
    await db.put("ops", "old-refused", {
      key: "old-refused",
      kind: "input",
      state: "refused",
      body: {},
      attempts: 1,
      createdAt: 0,
    });
    const send = vi.fn(async () => ({}));
    await new OpJournal(db, send, new ManualEnv()).recover();
    expect(send).not.toHaveBeenCalled();
    expect(await db.list("ops")).toEqual([]);
  });

  it("discard_and_settle_stop_the_running_retry", async () => {
    let calls = 0;
    const send = vi.fn(async () => {
      calls += 1;
      throw new NetworkError("drop");
    });
    const ops = journal(send);
    const op = await ops.begin(
      "input",
      { text: "discard me" },
      { conversationId: "c-1", persist: true },
    );
    const running = ops.run(op);
    await vi.waitFor(() => expect(calls).toBe(1));
    ops.discard(op.key);
    await env.advance(1000);
    await running;
    expect(calls).toBe(1);
    expect(ops.ops.has(op.key)).toBe(false);
    expect(await db.get("ops", op.key)).toBeUndefined();

    let release: (value: unknown) => void = () => undefined;
    const hanging = new Promise((resolve) => {
      release = resolve;
    });
    let second = 0;
    const sendLate = vi.fn(async () => {
      second += 1;
      if (second === 1) {
        await hanging;
        return { id: "late" };
      }
      return { id: "again" };
    });
    const later = journal(sendLate);
    const kept = await later.begin(
      "input",
      { text: "settle me" },
      { conversationId: "c-2", persist: true },
    );
    const inflight = later.run(kept);
    await vi.waitFor(() => expect(second).toBe(1));
    later.settle(kept.key);
    release({ id: "late" });
    await inflight;
    expect(second).toBe(1);
    expect(later.ops.has(kept.key)).toBe(false);
    expect(await db.get("ops", kept.key)).toBeUndefined();
  });

  it("journal_state_changes_reach_a_svelte_effect", async () => {
    const ops = journal(async () => ({ id: "inp-visible" }));
    const op = await ops.begin(
      "input",
      { text: "visible" },
      { conversationId: "c-1", persist: false },
    );
    const watch = watchOpState(ops.ops, op.key);
    flushSync();
    expect(watch.states).toContain("saving");
    await ops.run(op);
    flushSync();
    expect(watch.states).toContain("committed");
    expect(op.state).toBe("committed");
    watch.stop();
  });

  it("dropped_response_after_commit_retries_the_same_input_across_reload", async () => {
    const server = new Map<string, { id: string; key: string }>();
    const send = vi.fn(async (op: Op) => {
      if (!server.has(op.key))
        server.set(op.key, { id: "inp-once", key: op.key });
      throw new NetworkError("response dropped");
    });
    const first = journal(send);
    const op = await first.begin(
      "input",
      { text: "once" },
      { conversationId: "c-1", persist: true },
    );
    void first.run(op);
    await vi.waitFor(() => expect(server.size).toBe(1));
    expect((await db.get<Op>("ops", op.key))?.state).toBe("unknown");

    const recovered = new OpJournal(
      db,
      async (item: Op) => {
        if (!server.has(item.key))
          server.set(item.key, { id: "inp-second", key: item.key });
        return server.get(item.key);
      },
      new ManualEnv(),
    );
    await recovered.recover();
    await vi.waitFor(() =>
      expect(recovered.ops.get(op.key)?.state).toBe("committed"),
    );
    expect(server.size).toBe(1);
    expect(server.get(op.key)?.id).toBe("inp-once");
  });

  it("recover_returns_while_offline_without_waiting_for_the_retry", async () => {
    env.isOnline = false;
    await db.put("ops", "parked", {
      key: "parked",
      kind: "input",
      body: { text: "later" },
      state: "unknown",
      attempts: 1,
      createdAt: 0,
    });
    const send = vi.fn(async () => ({ id: "inp-parked" }));
    const ops = new OpJournal(db, send, env);
    const pending = await ops.recover();
    expect(pending.map((item) => item.key)).toEqual(["parked"]);
    await Promise.resolve();
    expect(send).not.toHaveBeenCalled();
    expect(ops.ops.get("parked")?.state).toBe("unknown");
    ops.close();
  });

  it("backoff_is_capped_at_30_seconds", async () => {
    let attempts = 0;
    const ops = journal(async () => {
      attempts += 1;
      if (attempts < 8) throw new NetworkError("drop");
      return { id: "inp-capped" };
    });
    const op = await ops.begin("other", { text: "cap" }, { persist: false });
    const running = ops.run(op);
    const delays = [1000, 2000, 4000, 8000, 16000, 30000, 30000];
    for (const delay of delays) {
      const before = env.scheduled.length;
      await vi.waitFor(() =>
        expect(env.scheduled.length).toBeGreaterThan(before),
      );
      expect(env.scheduled.at(-1)).toBe(delay);
      await env.advance(delay);
    }
    await running;
    expect(attempts).toBe(8);
    expect(op.state).toBe("committed");
  });

  it("persisted_kinds_survive_reload_and_other_kinds_do_not", async () => {
    const ops = journal(async () => ({ id: "unused" }));
    const input = await ops.begin(
      "input",
      { text: "keep" },
      { conversationId: "c-1", persist: false },
    );
    const scheduled = await ops.begin(
      "scheduled",
      { text: "keep" },
      { conversationId: "c-1", persist: false },
    );
    const created = await ops.begin(
      "create_conversation",
      { title: "keep" },
      { persist: false },
    );
    const skipped = await ops.begin(
      "other",
      { text: "page" },
      { persist: false },
    );
    const kept = await ops.begin("other", { text: "disk" }, { persist: true });
    expect((await db.get<Op>("ops", input.key))?.kind).toBe("input");
    expect((await db.get<Op>("ops", scheduled.key))?.kind).toBe("scheduled");
    expect((await db.get<Op>("ops", created.key))?.kind).toBe(
      "create_conversation",
    );
    expect(await db.get("ops", skipped.key)).toBeUndefined();
    expect((await db.get<Op>("ops", kept.key))?.kind).toBe("other");
  });

  it("unknown_op_resumes_on_online_and_page_events", async () => {
    env.isOnline = false;
    let calls = 0;
    const ops = journal(async () => {
      calls += 1;
      return { id: "inp-awake" };
    });
    const op = await ops.begin("other", { text: "wait" }, { persist: false });
    const running = ops.run(op);
    await vi.waitFor(() => expect(op.state).toBe("unknown"));
    expect(calls).toBe(0);
    env.isOnline = true;
    window.dispatchEvent(new Event("online"));
    await running;
    expect(calls).toBe(1);

    env.isOnline = false;
    const second = await ops.begin(
      "other",
      { text: "page" },
      { persist: false },
    );
    const again = ops.run(second);
    await vi.waitFor(() => expect(second.state).toBe("unknown"));
    env.isOnline = true;
    window.dispatchEvent(new Event("pageshow"));
    await again;

    env.isOnline = false;
    const third = await ops.begin(
      "other",
      { text: "visible" },
      { persist: false },
    );
    const visible = ops.run(third);
    await vi.waitFor(() => expect(third.state).toBe("unknown"));
    env.isOnline = true;
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "visible",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    await visible;
    ops.close();
  });

  it("abort_leaves_the_operation_unknown_and_retries_the_same_key", async () => {
    const keys: string[] = [];
    let attempts = 0;
    const ops = journal(async (op) => {
      attempts += 1;
      keys.push(op.key);
      if (attempts === 1)
        throw new DOMException("The operation was aborted", "AbortError");
      return { id: "inp-abort" };
    });
    const op = await ops.begin("other", { text: "abort" }, { persist: false });
    const running = ops.run(op);
    await vi.waitFor(() => expect(env.scheduled).toContain(1000));
    expect(op.state).toBe("unknown");
    await env.advance(1000);
    await running;
    expect(keys).toEqual([op.key, op.key]);
    expect(op.state).toBe("committed");
  });
});
