import { SvelteMap } from "svelte/reactivity";
import type { Idb } from "#lib/storage/idb.js";
import {
  ApiError,
  NetworkError,
  AuthRequired,
  session,
} from "#lib/api/http.js";
import { reactiveOp } from "#lib/ops/op.svelte.js";

export type OpKind = "input" | "scheduled" | "create_conversation" | "other";

export interface Op<B = unknown> {
  key: string;
  kind: OpKind;
  conversationId?: string;
  body: B;
  state: "saving" | "committed" | "refused" | "unknown";
  attempts: number;
  createdAt: number;
  refusal?: { code: string; message: string };
  result?: unknown;
}

export interface JournalEnv {
  now(): number;
  schedule(fn: () => void, ms: number): () => void;
  online(): boolean;
  authExpired(): boolean;
}

const openJournals = new Set<OpJournal>();

export function closeOpenJournals(): void {
  for (const journal of [...openJournals]) journal.retire();
}

export class OpJournal {
  readonly db: Idb;
  readonly ops: ReadonlyMap<string, Op> = new SvelteMap();

  #send: (op: Op) => Promise<unknown>;
  #env: JournalEnv;
  #persisted = new Set<string>();
  #beginning = new Set<string>();
  #running = new Set<string>();
  #stopped = new Set<string>();
  #wake = new Map<string, () => void>();
  #onOnline = () => this.#resumeWaiting();
  #onPageShow = () => this.#resumeWaiting();
  #onVisible = () => {
    if (
      typeof document !== "undefined" &&
      document.visibilityState === "visible"
    )
      this.#resumeWaiting();
  };

  constructor(db: Idb, send: (op: Op) => Promise<unknown>, env: JournalEnv) {
    this.db = db;
    this.#send = send;
    this.#env = env;
    openJournals.add(this);
    if (typeof window !== "undefined") {
      window.addEventListener("online", this.#onOnline);
      window.addEventListener("pageshow", this.#onPageShow);
      document.addEventListener("visibilitychange", this.#onVisible);
    }
  }

  close(): void {
    openJournals.delete(this);
    if (typeof window === "undefined") return;
    window.removeEventListener("online", this.#onOnline);
    window.removeEventListener("pageshow", this.#onPageShow);
    document.removeEventListener("visibilitychange", this.#onVisible);
  }

  retire(): void {
    for (const key of this.ops.keys()) this.#stop(key);
    this.close();
  }

  async begin<B>(
    kind: OpKind,
    body: B,
    opts: { conversationId?: string; persist: boolean; key?: string },
  ): Promise<Op<B>> {
    const op = reactiveOp<B>({
      key: opts.key ?? crypto.randomUUID(),
      kind,
      conversationId: opts.conversationId,
      body,
      state: "saving",
      attempts: 0,
      createdAt: this.#env.now(),
    });
    (this.ops as SvelteMap<string, Op>).set(op.key, op);
    if (kind !== "other" || opts.persist) {
      this.#persisted.add(op.key);
      this.#beginning.add(op.key);
      try {
        await this.#persist(op);
      } catch (err) {
        // Вызывающий получит отказ и вернёт текст: живая запись ушла бы на хаб второй копией.
        (this.ops as SvelteMap<string, Op>).delete(op.key);
        this.#persisted.delete(op.key);
        throw err;
      } finally {
        this.#beginning.delete(op.key);
      }
    }
    return op;
  }

  async run(op: Op): Promise<void> {
    if (this.#running.has(op.key) || this.#stopped.has(op.key)) return;
    this.#running.add(op.key);
    try {
      while (
        !this.#stopped.has(op.key) &&
        (op.state === "saving" || op.state === "unknown")
      ) {
        if (this.#authBlocked()) {
          op.state = "unknown";
          await this.#persist(op);
          if (this.#stopped.has(op.key)) return;
          await this.#wait(op.key, () => !this.#authBlocked());
          if (this.#stopped.has(op.key)) return;
          continue;
        }
        if (!this.#env.online()) {
          op.state = "unknown";
          await this.#persist(op);
          if (this.#stopped.has(op.key)) return;
          await this.#wait(op.key, () => this.#env.online());
          if (this.#stopped.has(op.key)) return;
          continue;
        }
        op.attempts += 1;
        op.state = "unknown";
        // Запись уже лежит в журнале с begin, а recover одинаково повторяет saving и unknown:
        // транзакция пометки создана до запроса, и IndexedDB выполнит её раньше следующих записей.
        void this.#persist(op).catch(() => undefined);
        try {
          const result = await this.#send(op);
          if (this.#stopped.has(op.key)) return;
          op.result = result;
          op.state = "committed";
          await this.#persist(op);
          if (this.#stopped.has(op.key)) return;
          return;
        } catch (err) {
          if (this.#stopped.has(op.key)) return;
          if (err instanceof AuthRequired) {
            op.state = "unknown";
            await this.#persist(op);
            if (this.#stopped.has(op.key)) return;
            await this.#wait(op.key, () => !this.#authBlocked());
            if (this.#stopped.has(op.key)) return;
            continue;
          }
          if (err instanceof ApiError && err.code === "idempotency_conflict") {
            op.state = "refused";
            op.refusal = { code: err.code, message: err.message };
            await this.#persist(op);
            throw err;
          }
          if (this.#retryable(err)) {
            op.state = "unknown";
            await this.#persist(op);
            if (this.#stopped.has(op.key)) return;
            await this.#backoff(op);
            if (this.#stopped.has(op.key)) return;
            continue;
          }
          if (err instanceof ApiError) {
            op.state = "refused";
            op.refusal = { code: err.code, message: err.message };
            await this.#drop(op.key);
            throw err;
          }
          throw err;
        }
      }
    } finally {
      this.#running.delete(op.key);
      this.#wake.delete(op.key);
    }
  }

  async recover(): Promise<Op[]> {
    const rows = await this.db.list<Op>("ops");
    const pending = rows.filter(
      (op) => op?.key && (op.state === "saving" || op.state === "unknown"),
    );
    // Завершённые строки повторять нечего: тела решений и сообщений на устройстве не копятся.
    for (const row of rows)
      if (row?.key && (row.state === "committed" || row.state === "refused"))
        await this.db.delete("ops", row.key);
    for (const row of pending) {
      const op = reactiveOp(row);
      this.#persisted.add(op.key);
      (this.ops as SvelteMap<string, Op>).set(op.key, op);
      pending[pending.indexOf(row)] = op;
    }
    for (const op of pending) void this.run(op).catch(() => undefined);
    return pending;
  }

  settle(key: string): void {
    this.#stop(key);
  }

  retryNow(key: string): void {
    const op = this.ops.get(key);
    if (!op || op.state === "committed" || op.state === "refused") return;
    op.state = "unknown";
    const wake = this.#wake.get(key);
    if (wake) wake();
    else void this.run(op);
  }

  discard(key: string): void {
    this.#stop(key);
  }

  #stop(key: string): void {
    this.#stopped.add(key);
    this.#wake.get(key)?.();
    (this.ops as SvelteMap<string, Op>).delete(key);
    this.#persisted.delete(key);
    void this.db.delete("ops", key);
  }

  #resumeWaiting(): void {
    for (const op of this.ops.values()) {
      // Операцию в записи begin запускает её вызывающий: только он вернёт текст при отказе.
      if (this.#beginning.has(op.key)) continue;
      if (op.state === "saving" || op.state === "unknown")
        this.retryNow(op.key);
    }
  }

  #authBlocked(): boolean {
    return session.state === "expired" || this.#env.authExpired();
  }

  #retryable(err: unknown): boolean {
    if (err instanceof NetworkError) return true;
    if (
      err !== null &&
      typeof err === "object" &&
      (err as { name?: string }).name === "AbortError"
    )
      return true;
    return err instanceof ApiError && err.retry === "safe_same_key";
  }

  async #persist(op: Op): Promise<void> {
    if (this.#stopped.has(op.key) || !this.#persisted.has(op.key)) return;
    const refusal = op.refusal;
    // Прокси $state IndexedDB не клонирует, поэтому в базу уходит обычный снимок.
    const snapshot = JSON.parse(
      JSON.stringify({
        key: op.key,
        kind: op.kind,
        conversationId: op.conversationId,
        body: op.body,
        state: op.state,
        attempts: op.attempts,
        createdAt: op.createdAt,
        refusal: refusal
          ? { code: refusal.code, message: refusal.message }
          : undefined,
      }),
    ) as Op;
    try {
      await this.db.put("ops", op.key, snapshot);
    } catch (err) {
      if (err instanceof Error && err.message === "storage_retired") return;
      throw err;
    }
    if (this.#stopped.has(op.key)) await this.db.delete("ops", op.key);
  }

  async #drop(key: string): Promise<void> {
    (this.ops as SvelteMap<string, Op>).delete(key);
    this.#persisted.delete(key);
    await this.db.delete("ops", key);
  }

  async #backoff(op: Op): Promise<void> {
    const ms = Math.min(1000 * 2 ** Math.max(0, op.attempts - 1), 30_000);
    await this.#wait(op.key, () => false, ms);
  }

  #wait(key: string, ready: () => boolean, ms?: number): Promise<void> {
    if (ready()) return Promise.resolve();
    return new Promise((resolve) => {
      let cancel = () => {};
      const finish = () => {
        cancel();
        this.#wake.delete(key);
        resolve();
      };
      this.#wake.set(key, finish);
      if (ms !== undefined) {
        cancel = this.#env.schedule(finish, ms);
        return;
      }
      const poll = () => {
        if (ready()) finish();
        else cancel = this.#env.schedule(poll, 50);
      };
      cancel = this.#env.schedule(poll, 50);
    });
  }
}
