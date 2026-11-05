import {
  conversationBlock,
  conversationHistory,
  conversationInputs,
  conversationLane,
  conversationSnapshot,
  eventsRange,
  markRead,
} from "#lib/api/client.js";
import { browserRuntime, onOwnerCleanup, onRuntimeReady } from "#lib/app/browser.js";
import { getRuntime, type Runtime } from "#lib/app/runtime.js";
import { probe } from "#lib/perf/probe.js";
import { UploadManager } from "#lib/uploads/manager.js";
import { sha256Hex, type BlockPage } from "#lib/stores/blocks.js";
import {
  ChatStore,
  type HistoryPage,
  type LanePage,
  type SnapshotData,
} from "#lib/stores/chat.svelte.js";
import type { RangeResult } from "#lib/sync/scope.js";

/** Сколько отпущенных чатов держать готовыми к возврату; остальные освобождаются целиком. */
const WARM_CHATS = 4;

const chats = new Map<string, ChatStore>();
const opening = new Map<string, Promise<ChatStore>>();
const holds = new Map<string, number>();
let uploads: UploadManager | null = null;
let deletionBound: Runtime | null = null;

/** Удалённая сессия: открытый экран покажет это, а тёплая копия и загрузки исчезают сразу. */
function bindDeletion(runtime: Runtime): void {
  if (deletionBound === runtime) return;
  deletionBound = runtime;
  runtime.list.onDeleted((id) => {
    const chat = chats.get(id);
    chat?.markDeleted();
    if (chat && !holds.has(id)) {
      chats.delete(id);
      chat.dispose();
    }
    void uploads?.cancelConversation(id);
  });
}

onRuntimeReady(bindDeletion);

onOwnerCleanup(() => {
  for (const chat of chats.values()) chat.dispose();
  chats.clear();
  opening.clear();
  uploads?.dispose();
  uploads = null;
  deletionBound = null;
});

export function sessionUploads(): UploadManager {
  const runtime = getRuntime();
  if (!uploads || uploads.db !== runtime.db) {
    uploads = new UploadManager({ db: runtime.db });
    void uploads.resumePending();
  }
  return uploads;
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
      const chat = chats.get(id);
      chat?.close();
      if (chat?.deleted) {
        chats.delete(id);
        chat.dispose();
      }
      trimChats();
    },
  };
}

function trimChats(): void {
  for (const [id, chat] of chats) {
    if (chats.size <= WARM_CHATS) return;
    if (holds.has(id)) continue;
    chats.delete(id);
    chat.dispose();
  }
}

function openChatSession(id: string): Promise<ChatStore> {
  const existing = chats.get(id);
  if (existing) {
    chats.delete(id);
    try {
      existing.open();
    } catch (error) {
      existing.dispose();
      return Promise.reject(error);
    }
    chats.set(id, existing);
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
        const { cursor, data } = response.data;
        const snapshot: SnapshotData = {
          ...data,
          history: data.history as HistoryPage,
        };
        return { cursor, data: snapshot };
      },
      async history(conversationId, query) {
        const response = await conversationHistory(conversationId, query);
        return response.data as HistoryPage;
      },
      async inputs(conversationId, query) {
        const response = await conversationInputs(conversationId, query);
        return response.data;
      },
      async lane(conversationId, blockId, query) {
        const response = await conversationLane(conversationId, blockId, query);
        return response.data as LanePage;
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
  try {
    chat.open();
  } catch (error) {
    chat.dispose();
    throw error;
  }
  chats.set(id, chat);
  if (!holds.has(id)) chat.close();
  trimChats();
  return chat;
}
