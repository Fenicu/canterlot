import {
  conversationBlock,
  conversationHistory,
  conversationSnapshot,
  eventsRange,
  markRead,
} from "#lib/api/client.js";
import { createRuntime, getRuntime, type Runtime } from "#lib/app/runtime.js";
import { probe } from "#lib/perf/probe.js";
import { UploadManager } from "#lib/uploads/manager.js";
import { sha256Hex, type BlockPage } from "#lib/stores/blocks.js";
import {
  ChatStore,
  type HistoryPage,
  type SnapshotData,
} from "#lib/stores/chat.svelte.js";
import type { RangeResult } from "#lib/sync/scope.js";
import { onOwnerRetired } from "#lib/storage/owner.js";
import { resetConfig } from "#lib/api/config.js";
import { restoreComposerText } from "#lib/composer/restore.js";

let pending: Promise<Runtime> | null = null;
const chats = new Map<string, ChatStore>();
const opening = new Map<string, Promise<ChatStore>>();
const holds = new Map<string, number>();
let uploads: UploadManager | null = null;
let cleanupBound = false;

function bindCleanup(): void {
  if (cleanupBound) return;
  cleanupBound = true;
  onOwnerRetired(() => {
    restoreComposerText("");
    for (const chat of chats.values()) chat.dispose();
    chats.clear();
    opening.clear();
    uploads?.dispose();
    uploads = null;
    pending = null;
    resetConfig();
    cleanupBound = false;
  });
}

export function sessionUploads(): UploadManager {
  const runtime = getRuntime();
  if (!uploads || uploads.db !== runtime.db) {
    uploads = new UploadManager({ db: runtime.db });
    void uploads.resumePending();
  }
  return uploads;
}

export function browserRuntime(): Promise<Runtime> {
  bindCleanup();
  try {
    return Promise.resolve(getRuntime());
  } catch {
    pending ??= createRuntime({
      fetch: globalThis.fetch.bind(globalThis),
      WebSocket: globalThis.WebSocket,
      indexedDB: globalThis.indexedDB,
      clock: { now: () => Date.now() },
      schedule: (fn, ms) => {
        const id = setTimeout(fn, ms);
        return () => clearTimeout(id);
      },
    });
    return pending
      .then((runtime) => {
        bindCleanup();
        return runtime;
      })
      .catch((error) => {
        pending = null;
        throw error;
      });
  }
}

export interface ChatLease {
  readonly chat: Promise<ChatStore>;
  release(): void;
}

/** Экран держит подписку сессии, пока смонтирован; после release она закрывается. */
export function leaseChatSession(id: string): ChatLease {
  holds.set(id, (holds.get(id) ?? 0) + 1);
  let released = false;
  return {
    chat: openChatSession(id),
    release() {
      if (released) return;
      released = true;
      const left = (holds.get(id) ?? 1) - 1;
      if (left > 0) {
        holds.set(id, left);
        return;
      }
      holds.delete(id);
      chats.get(id)?.close();
    },
  };
}

function openChatSession(id: string): Promise<ChatStore> {
  const existing = chats.get(id);
  if (existing) {
    existing.open();
    return Promise.resolve(existing);
  }
  let creating = opening.get(id);
  if (!creating) {
    creating = createChatSession(id).finally(() => opening.delete(id));
    opening.set(id, creating);
  }
  return creating;
}

async function createChatSession(id: string): Promise<ChatStore> {
  const runtime = await browserRuntime();
  const chat = new ChatStore(id, {
    api: {
      async conversationSnapshot(conversationId) {
        const response = await conversationSnapshot(conversationId);
        const body = response.data as unknown as {
          cursor: string;
          data: SnapshotData;
        };
        return { cursor: body.cursor, data: body.data };
      },
      async history(conversationId, query) {
        const response = await conversationHistory(conversationId, query);
        return response.data as HistoryPage;
      },
      async events(scope, after, limit) {
        const response = await eventsRange({ scope, after, limit });
        const body = response.data as {
          events?: RangeResult extends { events: infer E } ? E : never;
          head?: string;
          resync_required?: unknown;
        };
        if (body && typeof body === "object" && "resync_required" in body)
          return { resync: true };
        return { events: body.events ?? [], head: body.head ?? after };
      },
      async markRead(conversationId, seq) {
        await markRead(conversationId, { seq });
      },
    },
    ops: runtime.journal,
    drafts: runtime.drafts,
    connection: runtime.connection,
    blockIO: {
      async fetchContent(blockId, offset): Promise<BlockPage> {
        const response = await conversationBlock(id, blockId, { offset });
        return response.data as BlockPage;
      },
      sha256Hex,
    },
    clock: { now: () => Date.now() },
    probe,
  });
  chats.set(id, chat);
  chat.open();
  if (!holds.has(id)) chat.close();
  return chat;
}
