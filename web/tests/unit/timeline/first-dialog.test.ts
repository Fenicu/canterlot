import { describe, expect, it } from "vitest";
import { modelLabel } from "#lib/format/labels.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { OpJournal } from "#lib/ops/journal.js";
import { ChatStore, type ChatApi, type HistoryBlock } from "#lib/stores/chat.svelte.js";
import type { BlockModel } from "#lib/stores/blocks.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import {
  agentTitle,
  subagentCopy,
  toolStatusText,
  toolTiming,
  toolTitle,
} from "#lib/timeline/present.js";
import { blockLinks } from "#lib/timeline/rows.js";
import { FIRST_DIALOG, firstDialogBlock } from "../../../testing/fixtures/first-dialog";

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

const calls = (chat: ChatStore, tool: string): BlockModel[] =>
  [...chat.blocks.values()].filter((block) => block.toolName === tool);

function pill(chat: ChatStore, call: BlockModel, working = false): string {
  const links = blockLinks(chat.timeline.entries);
  const result = links.result(call.id);
  const timing = toolTiming(call, result, working);
  return toolStatusText({ ...timing, error: false, now: Date.parse("2026-10-31T11:00:00Z") });
}

describe("first dialog: durations", () => {
  it("come_from_the_call_to_its_result", () => {
    const chat = loaded();
    expect(pill(chat, calls(chat, "ToolSearch")[0]!)).toBe("26 мс");
    expect(pill(chat, calls(chat, "AskUserQuestion")[0]!)).toBe("43 с");
    expect(pill(chat, calls(chat, "Bash")[0]!)).toBe("0,1 с");
  });

  it("background_sleep_is_as_short_as_the_tool_call_really_was", () => {
    const chat = loaded();
    const launched = calls(chat, "Bash").at(-1)!;
    expect(launched.summary).toBe("Bash: sleep 90");
    expect(pill(chat, launched)).toBe("14 мс");
  });

  it("call_without_a_result_is_live_while_the_turn_runs_and_silent_after", () => {
    const call = firstDialogBlock((block) => block.tool_name === "Read");
    const chat = loaded(FIRST_DIALOG.filter((item) => item.kind === "block" && item.id === call.id));
    const only = calls(chat, "Read")[0]!;
    expect(toolTiming(only, null, true).running).toBe(true);
    expect(pill(chat, only, false)).toBe("");
  });

  it("call_that_keeps_its_own_time_shows_it_without_a_result", () => {
    const call = firstDialogBlock((block) => block.tool_name === "Read");
    const own: HistoryBlock = { ...call, finished_at: "2026-10-31T10:21:58.368Z" };
    const chat = loaded([own]);
    expect(pill(chat, calls(chat, "Read")[0]!)).toBe("2 с");
  });

  it("rule_allowed_call_has_no_true_start_and_shows_nothing", () => {
    const chat = loaded();
    const call = calls(chat, "Read")[0]!;
    call.approvedBy = "cli_policy";
    expect(pill(chat, call)).toBe("");
  });
});

describe("first dialog: names", () => {
  it("main_agent_is_shown_as_the_harness", () => {
    const claude = { title: "Claude Code", provider: "claude" };
    expect(agentTitle("orchestrator", claude)).toBe("Claude");
    expect(agentTitle("orchestrator", { title: "Codex", provider: "codex" })).toBe("Codex");
    expect(agentTitle("orchestrator", { title: "OpenCode", provider: "opencode" })).toBe("OpenCode");
    expect(agentTitle("orchestrator", null)).toBe("Агент");
    expect(agentTitle("reviewer", claude)).toBe("Reviewer");
  });

  it("subagent_row_takes_the_name_from_its_own_blocks_and_never_the_internal_label", () => {
    const chat = loaded();
    const agent = calls(chat, "Agent")[0]!;
    const handBack = [...chat.blocks.values()].find((block) => block.parentId === agent.id)!;
    expect(subagentCopy(agent, [handBack]).title).toBe("Субагент · Opus 5.5");
    const explore = [...chat.blocks.values()].find((block) => block.agent === "Explore")!;
    const copy = subagentCopy(agent, [handBack, explore]);
    expect(copy.title).toBe("Субагент · Explore · Opus 5.5");
    expect(copy.detail).toBe("Работает · Find note.txt in check dir");
  });

  it("model_ids_read_as_product_names", () => {
    expect(modelLabel("claude-opus-5-5")).toBe("Opus 5.5");
    expect(modelLabel("claude-haiku-5-5")).toBe("Haiku 5.5");
    expect(modelLabel("claude-opus-4-6")).toBe("Opus 4.6");
    expect(modelLabel("claude-sonnet-4-5-20250929")).toBe("Sonnet 4.5");
    expect(modelLabel("opus[1m]")).toBe("Opus 1M");
    expect(modelLabel("openai/gpt-5.6")).toBe("gpt-5.6");
  });
});

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
