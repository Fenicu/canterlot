import { describe, expect, it } from "vitest";
import type { DraftStore } from "#lib/drafts/store.js";
import type { OpJournal } from "#lib/ops/journal.js";
import { ChatStore, type ChatApi } from "#lib/stores/chat.svelte.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import {
  toolTitle,
} from "#lib/timeline/present.js";
import { shownInCard } from "#lib/timeline/rows.js";
import { FIRST_DIALOG } from "../../../testing/fixtures/first-dialog";

const CONV = "01a19179-9c91-73e5-bfc8-f4cb774a303d";

function loaded(items = FIRST_DIALOG): ChatStore {
  const api: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: async () => ({ items }),
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
  };
  const chat = new ChatStore(CONV, {
    api,
    ops: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: { subscribe: () => ({ release() {}, status: "idle", cursor: "0" }) } as unknown as ConnectionManager,
    blockIO: {
      fetchContent: async () => ({ content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 }),
      sha256Hex: async () => "",
    },
    clock: { now: () => 0 },
    probe: {},
  });
  chat.applier.applySnapshot({ history: { items } } as never, "0");
  return chat;
}

describe("first dialog: tool titles", () => {
  it("name_and_argument_appear_once", () => {
    expect(toolTitle({ toolName: "Bash", summary: "Bash: sleep 90" })).toEqual({ name: "Bash", server: "", argument: "sleep 90" });
    expect(toolTitle({ toolName: "Read", summary: "Read: /tmp/canterlot-check/note.txt" }).argument).toBe("/tmp/canterlot-check/note.txt");
    expect(toolTitle({ toolName: "ToolSearch", summary: "ToolSearch" }).argument).toBe("");
    expect(toolTitle({ toolName: "AskUserQuestion", summary: null }).argument).toBe("");
    expect(toolTitle({ toolName: "Read", summary: "src/lib/timeline" }).argument).toBe("src/lib/timeline");
    expect(toolTitle({ toolName: "todo", summary: "todo, completed" }).argument).toBe("completed");
  });

  it("mcp_names_lose_the_technical_prefix", () => {
    const own = "mcp__canterlot__canterlot_limits";
    expect(toolTitle({ toolName: own, summary: own })).toEqual({ name: "canterlot_limits", server: "", argument: "" });
    expect(toolTitle({ toolName: "mcp__remnawave__users_get_users", summary: "mcp__remnawave__users_get_users" }))
      .toEqual({ name: "users_get_users", server: "remnawave", argument: "" });
    expect(toolTitle({ toolName: "mcp__plugin_context7_context7__query-docs", summary: null }).server).toBe("context7");
  });
});
