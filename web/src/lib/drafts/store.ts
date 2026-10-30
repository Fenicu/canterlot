import type { CanterlotDb } from "#lib/storage/idb.js";

export interface DraftAttachment {
  id: string;
  name?: string;
  size?: number;
}

export interface Draft {
  conversationId: string;
  text: string;
  attachments: DraftAttachment[];
  updatedAt: number;
  ownerId?: string;
}

export interface DraftStoreOptions {
  onPersistLag?: (lagMs: number) => void;
  debounceMs?: number;
}

interface PendingDraft {
  text: string;
  attachments: DraftAttachment[];
  updatedAt: number;
  pendingSince: number;
}

const writers = new Set<DraftStore>();

export function stopDraftWriters(): void {
  for (const writer of [...writers]) writer.retire();
}

export class DraftStore {
  readonly ownerId: string;
  readonly db: CanterlotDb;
  readonly options?: DraftStoreOptions;

  readonly lag = { lastMs: 0, maxMs: 0 };

  #pending = new Map<string, PendingDraft>();
  #known = new Map<string, Draft>();
  #inflight = new Map<string, Draft>();
  #writes = new Set<Promise<void>>();
  #retired = false;
  #timers = new Map<string, ReturnType<typeof setTimeout>>();
  #onVisibilityChange?: () => void;
  #onPageHide?: () => void;
  #onFreeze?: () => void;
  #composerStops: Array<() => void> = [];

  constructor(ownerId: string, db: CanterlotDb, options?: DraftStoreOptions) {
    this.ownerId = ownerId;
    this.db = db;
    this.options = options;
    writers.add(this);

    if (typeof document !== "undefined") {
      this.#onVisibilityChange = () => {
        if (document.visibilityState === "hidden") {
          void this.flush();
        }
      };
      document.addEventListener("visibilitychange", this.#onVisibilityChange);
    }

    if (typeof window !== "undefined") {
      this.#onPageHide = () => {
        this.flushSync();
      };
      window.addEventListener("pagehide", this.#onPageHide);
      this.#onFreeze = () => {
        void this.flush();
      };
      window.addEventListener("freeze", this.#onFreeze);
    }
  }

  #tailKey(conversationId: string): string {
    return `canterlot.draft-tail.${conversationId}`;
  }

  flushSync(): void {
    if (this.#retired || typeof localStorage === "undefined") return;
    const mirrored = new Set<string>();
    for (const [conversationId, pending] of this.#pending.entries()) {
      mirrored.add(conversationId);
      this.#mirror(
        conversationId,
        pending.text,
        pending.attachments,
        pending.updatedAt,
      );
    }
    for (const [conversationId, draft] of this.#inflight.entries()) {
      if (mirrored.has(conversationId)) continue;
      this.#mirror(
        conversationId,
        draft.text,
        draft.attachments,
        draft.updatedAt,
      );
    }
  }

  #mirror(
    conversationId: string,
    text: string,
    attachments: DraftAttachment[],
    updatedAt: number,
  ): void {
    const draft: Draft = {
      ownerId: this.ownerId,
      conversationId,
      text,
      attachments,
      updatedAt,
    };
    localStorage.setItem(this.#tailKey(conversationId), JSON.stringify(draft));
  }

  set(conversationId: string, patch: Partial<Draft>): void {
    if (this.#retired) return;
    const now = Date.now();
    const existing =
      this.#pending.get(conversationId) ?? this.#known.get(conversationId);
    const pendingSince = now;
    const pending: PendingDraft = {
      text: patch.text ?? existing?.text ?? "",
      attachments: patch.attachments ?? existing?.attachments ?? [],
      updatedAt: patch.updatedAt ?? now,
      pendingSince,
    };
    this.#pending.set(conversationId, pending);
    this.#known.set(conversationId, {
      ownerId: this.ownerId,
      conversationId,
      text: pending.text,
      attachments: pending.attachments,
      updatedAt: pending.updatedAt,
    });

    const existingTimer = this.#timers.get(conversationId);
    if (existingTimer) {
      clearTimeout(existingTimer);
    }

    const delay = this.options?.debounceMs ?? 250;
    const timer = setTimeout(() => {
      this.#timers.delete(conversationId);
      void this.#flushConversation(conversationId);
    }, delay);

    this.#timers.set(conversationId, timer);
  }

  async #flushConversation(conversationId: string): Promise<void> {
    if (this.#retired) return;
    const pending = this.#pending.get(conversationId);
    if (!pending) return;

    const draft: Draft = {
      ownerId: this.ownerId,
      conversationId,
      text: pending.text,
      attachments: pending.attachments,
      updatedAt: pending.updatedAt,
    };
    this.#inflight.set(conversationId, draft);
    if (this.#pending.get(conversationId) === pending)
      this.#pending.delete(conversationId);
    this.#known.set(conversationId, draft);

    const write = this.db.put("drafts", conversationId, draft);
    this.#writes.add(write);
    try {
      await write;
    } catch (err) {
      if (this.#retired) return;
      if (!this.#pending.has(conversationId))
        this.#pending.set(conversationId, pending);
      throw err;
    } finally {
      this.#writes.delete(write);
    }
    if (this.#inflight.get(conversationId) === draft)
      this.#inflight.delete(conversationId);

    const lag = Date.now() - pending.pendingSince;
    this.lag.lastMs = lag;
    this.lag.maxMs = Math.max(this.lag.maxMs, lag);
    this.options?.onPersistLag?.(lag);
  }

  async load(conversationId: string): Promise<Draft | null> {
    if (this.#retired) return null;
    let mirrorDraft: Draft | undefined;
    const tailKey = this.#tailKey(conversationId);
    if (typeof localStorage !== "undefined") {
      const raw = localStorage.getItem(tailKey);
      if (raw) {
        try {
          const parsed = JSON.parse(raw) as Draft;
          if (parsed.ownerId === this.ownerId) mirrorDraft = parsed;
          else localStorage.removeItem(tailKey);
        } catch {
          mirrorDraft = undefined;
        }
      }
    }
    const idbDraft = await this.db.get<Draft>("drafts", conversationId);
    if (this.#retired) return null;
    if (mirrorDraft) {
      if (typeof localStorage !== "undefined") localStorage.removeItem(tailKey);
      if (!idbDraft || mirrorDraft.updatedAt >= idbDraft.updatedAt) {
        await this.db.put("drafts", conversationId, mirrorDraft);
        if (this.#retired) return null;
        this.#known.set(conversationId, mirrorDraft);
        return mirrorDraft;
      }
    }
    const chosen = idbDraft ?? null;
    if (chosen) this.#known.set(conversationId, chosen);
    return chosen;
  }

  /** Возвращённое сообщение встаёт перед набранным и сразу уходит на диск: другой копии может не быть. */
  async prepend(
    conversationId: string,
    text: string,
    attachments: DraftAttachment[],
  ): Promise<void> {
    const current =
      this.#pending.get(conversationId) ??
      this.#known.get(conversationId) ??
      (await this.load(conversationId));
    this.set(conversationId, {
      text: current?.text ? `${text}\n${current.text}` : text,
      attachments: [...(current?.attachments ?? []), ...attachments],
    });
    await this.flush();
  }

  async clear(conversationId: string): Promise<void> {
    this.#pending.delete(conversationId);
    this.#known.delete(conversationId);
    this.#inflight.delete(conversationId);
    const timer = this.#timers.get(conversationId);
    if (timer) {
      clearTimeout(timer);
      this.#timers.delete(conversationId);
    }
    if (typeof localStorage !== "undefined")
      localStorage.removeItem(this.#tailKey(conversationId));
    await this.db.delete("drafts", conversationId);
  }

  watchComposer(node: EventTarget): () => void {
    const onBlur = () => {
      void this.flush();
    };
    node.addEventListener("blur", onBlur);
    const stop = () => {
      node.removeEventListener("blur", onBlur);
      this.#composerStops = this.#composerStops.filter((item) => item !== stop);
    };
    this.#composerStops.push(stop);
    return stop;
  }

  leave(): Promise<void> {
    return this.flush();
  }

  async flush(): Promise<void> {
    for (const timer of this.#timers.values()) {
      clearTimeout(timer);
    }
    this.#timers.clear();

    const convIds = Array.from(this.#pending.keys());
    await Promise.all(convIds.map((id) => this.#flushConversation(id)));
    await Promise.all(this.#writes);
  }

  get pending(): boolean {
    return this.#pending.size > 0 || this.#writes.size > 0;
  }

  destroy(): void {
    this.retire();
  }

  retire(): void {
    this.#retired = true;
    writers.delete(this);
    for (const timer of this.#timers.values()) clearTimeout(timer);
    this.#timers.clear();
    this.#pending.clear();
    this.#inflight.clear();
    this.#known.clear();
    this.#detach();
  }

  #detach(): void {
    if (this.#onVisibilityChange && typeof document !== "undefined") {
      document.removeEventListener(
        "visibilitychange",
        this.#onVisibilityChange,
      );
    }
    if (this.#onPageHide && typeof window !== "undefined") {
      window.removeEventListener("pagehide", this.#onPageHide);
    }
    if (this.#onFreeze && typeof window !== "undefined") {
      window.removeEventListener("freeze", this.#onFreeze);
    }
    for (const stop of [...this.#composerStops]) stop();
  }
}
