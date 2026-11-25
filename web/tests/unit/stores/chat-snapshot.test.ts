import { describe, expect, it } from "vitest";
import type { OpJournal } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import { sha256Hex } from "#lib/stores/blocks.js";
import { FakeClock } from "../../../testing/fake-hub";
import { ChatStore, type ChatApi, type HistoryItem, type SnapshotData } from "#lib/stores/chat.svelte.js";
import type { HubEvent } from "#lib/sync/wire.js";

const CONV = "00000000-0000-4000-8000-0000000000aa";

export function ev(kind: string, payload: unknown, seq = "1", execution: string | null = null): HubEvent {
  return {
    v: 1,
    id: `00000000-0000-4000-8000-${seq.padStart(12, "0")}`,
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: `conversation:${CONV}`,
    scope_seq: seq,
    scope_seq_from: seq,
    committed_at: "2026-10-28T12:00:00.000Z",
    committed_at_from: "2026-10-28T12:00:00.000Z",
    conversation_id: CONV,
    execution_id: execution,
    kind,
    payload,
  } as HubEvent;
}

export function chatOf(
  api: Partial<ChatApi> = {},
  resync: () => void = () => undefined,
  clock: { now(): number; schedule?: (fn: () => void, ms: number) => () => void } = { now: () => 0 },
): ChatStore {
  const full: ChatApi = {
    conversationSnapshot: async () => ({ cursor: "0", data: {} }),
    history: async () => ({ items: [] }),
    events: async () => ({ events: [], head: "0" }),
    markRead: async () => undefined,
    ...api,
  };
  return new ChatStore(CONV, {
    api: full,
    ops: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined } as unknown as OpJournal,
    drafts: {} as DraftStore,
    connection: { subscribe: () => ({ release() {}, resync, status: "idle", cursor: "0" }) } as unknown as ConnectionManager,
    blockIO: {
      fetchContent: async () => ({ content: "", offset: 0, rev: 0, state: "streaming", total_bytes: 0 }),
      sha256Hex,
    },
    clock,
    probe: {},
  });
}

function snapshot(chat: ChatStore, data: SnapshotData): void {
  chat.applier.applySnapshot(data, "0");
}

function apply(chat: ChatStore, ...events: HubEvent[]): void {
  chat.applier.applyEvents(events.map((event) => ({ event, overlap: false })));
}

const permission = (id: string) => ({
  id,
  conversation_id: CONV,
  kind: "permission",
  state: "open",
  request_payload: { command: "ls" },
});

describe("снимок тёплого чата", () => {
  it("снимок без живого исполнения снимает старое исполнение, фазу и прерывание", () => {
    const chat = chatOf();
    snapshot(chat, { detail: { execution: { execution_id: "e1", state: "running" } } });
    apply(
      chat,
      ev("phase_changed", { phase: "context_maintenance" }, "2"),
      ev("interrupt_state_changed", { status: "requested" }, "3"),
    );
    snapshot(chat, { detail: { execution: null } });
    expect(chat.execution).toBeNull();
    expect(chat.phase).toBeNull();
    expect(chat.interrupt).toBeNull();
  });

  it("запрос, закрытый вдали от чата, уходит из открытых", () => {
    const chat = chatOf();
    snapshot(chat, { open_requests: [permission("r1"), permission("r2")] });
    expect(chat.requests.items.get("r1")!.state).toBe("open");
    snapshot(chat, { open_requests: [permission("r2")] });
    expect(chat.requests.items.get("r1")!.state).toBe("closed");
    expect(chat.requests.items.get("r2")!.state).toBe("open");
  });

  it("ответ, который владелец ещё отправляет, снимок не трогает", () => {
    const chat = chatOf();
    snapshot(chat, { open_requests: [permission("r1")] });
    chat.requests.items.get("r1")!.state = "sending";
    snapshot(chat, { open_requests: [] });
    expect(chat.requests.items.get("r1")!.state).toBe("sending");
  });

  it("события, удержанные до нового снимка, не применяются поверх него", async () => {
    let release: ((page: { items: HistoryItem[] }) => void) | undefined;
    const chat = chatOf({
      history: () => new Promise((resolve) => { release = resolve; }),
    });
    const pending = chat.reloadTail();
    apply(chat, ev("request_opened", { request_id: "r1", kind: "permission", payload: {} }, "5"));
    snapshot(chat, { open_requests: [] });
    release?.({ items: [] });
    await pending;
    expect(chat.requests.items.get("r1")).toBeUndefined();
  });

  it("два наложившихся перечитывания хвоста держат события до конца второго", async () => {
    const releases: ((page: { items: HistoryItem[] }) => void)[] = [];
    const chat = chatOf({
      history: () => new Promise((resolve) => { releases.push(resolve); }),
    });
    const first = chat.reloadTail();
    const second = chat.reloadTail();
    releases[0]!({ items: [] });
    await first;
    const id = "00000000-0000-4000-8000-00000000fffe";
    apply(chat, ev("block_started", { block_id: id, rev: 0, kind: "text", input_command_id: null, parent_block_id: null, agent: "a", model: "m", tool_name: null, summary: null, approved_by: null }, "4"));
    expect(chat.blocks.get(id)).toBeUndefined();
    releases[1]!({ items: [] });
    await second;
    expect(chat.blocks.get(id)).toBeDefined();
  });
});

describe("прерывание хода и открытые запросы", () => {
  const request = (id: string, kind: string) => ({ id, conversation_id: CONV, kind, state: "open", request_payload: {} });
  const states = (chat: ChatStore): Record<string, string> =>
    Object.fromEntries([...chat.requests.items.values()].map((item) => [item.id, item.state]));

  it("запросы закрывает событие хаба, а не само прерывание", () => {
    const chat = chatOf();
    snapshot(chat, {
      open_requests: [
        request("r-perm", "permission"),
        request("r-ask", "question"),
        request("r-spawn", "spawn_approval"),
        request("r-set", "settings_change"),
        request("r-cloud", "cloud_window"),
      ],
    });
    apply(chat, ev("interrupt_state_changed", { command_id: "c1", status: "acknowledged" }, "5"));
    apply(chat, ev("interrupt_state_changed", { command_id: "c1", status: "completed" }, "6"));
    // Одобрение запуска, настройку и облачное окно хаб при прерывании не трогает: они ждут ответа.
    expect(states(chat)).toEqual({
      "r-perm": "open",
      "r-ask": "open",
      "r-spawn": "open",
      "r-set": "open",
      "r-cloud": "open",
    });

    // Запросы самого хода закрывает машина: на каждый приходит request_closed.
    apply(
      chat,
      ev("request_closed", { request_id: "r-perm", reason: "interrupted" }, "7"),
      ev("request_closed", { request_id: "r-ask", reason: "interrupted" }, "8"),
    );
    expect(states(chat)).toEqual({
      "r-perm": "closed",
      "r-ask": "closed",
      "r-spawn": "open",
      "r-set": "open",
      "r-cloud": "open",
    });
    expect(chat.requests.items.get("r-perm")!.closedReason).toBe("interrupted");
  });
});

describe("цена хода: событие новее ответа истории", () => {
  const RUN = "00000000-0000-4000-8000-0000000000b1";
  const usage = (cost: number) => [
    { model: "m", agent: "main", input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cost_micro_usd: cost, cost_source: "cli" },
  ];
  const costOf = (chat: ChatStore) => chat.runCosts.get(RUN)?.usage[0]?.cost_micro_usd;

  it("run_cost_updated во время запроса истории не затирается её старым значением", async () => {
    let release: ((page: { items: HistoryItem[]; runs?: unknown[] }) => void) | undefined;
    const chat = chatOf({
      history: () => new Promise((resolve) => { release = resolve as typeof release; }),
    });
    const pending = chat.reloadTail();
    apply(chat, ev("run_cost_updated", { run_id: RUN, usage: usage(900), limit: null, cache_hint: null }, "7"));
    release?.({ items: [], runs: [{ run_id: RUN, usage: usage(100), limit: null }] });
    await pending;
    expect(costOf(chat)).toBe(900);
  });

  it("запрос, начатый после события, его значение заменяет", async () => {
    const chat = chatOf({
      history: async () => ({ items: [], runs: [{ run_id: RUN, usage: usage(1200), limit: null }] }),
    });
    apply(chat, ev("run_cost_updated", { run_id: RUN, usage: usage(900), limit: null, cache_hint: null }, "7"));
    await chat.reloadTail();
    expect(costOf(chat)).toBe(1200);
  });

  it("старая страница не затирает событие и при подгрузке старых", async () => {
    let release: ((page: { items: HistoryItem[]; runs?: unknown[] }) => void) | undefined;
    const chat = chatOf({
      history: () => new Promise((resolve) => { release = resolve as typeof release; }),
    });
    const pending = chat.loadOlder();
    apply(chat, ev("run_cost_updated", { run_id: RUN, usage: usage(900), limit: null, cache_hint: null }, "7"));
    release?.({ items: [], runs: [{ run_id: RUN, usage: usage(100), limit: null }] });
    await pending;
    expect(costOf(chat)).toBe(900);
  });
});

function blockItem(seq: number): HistoryItem {
  return {
    kind: "block",
    id: `00000000-0000-4000-8000-${String(seq).padStart(12, "0")}`,
    block_kind: "text",
    content: `строка ${seq}`,
    content_bytes: 10,
    content_truncated: false,
    created_seq: String(seq),
    created_at: "2026-10-28T12:00:00.000Z",
    rev: 1,
    state: "final",
  };
}

/** Хаб с длинной историей: страницы по сотне, как у настоящего. */
function longHistory(total: number): ChatApi["history"] {
  const all = Array.from({ length: total }, (_, index) => blockItem(index + 1));
  return async (_id, query = {}) => {
    let list = all;
    if (query.before) list = list.filter((item) => Number(item.created_seq) < Number(query.before));
    if (query.after) list = list.filter((item) => Number(item.created_seq) > Number(query.after));
    const slice = query.after ? list.slice(0, 100) : list.slice(-100);
    const first = slice[0];
    const last = slice.at(-1);
    return {
      items: slice,
      next_before: first && Number(first.created_seq) > 1 ? first.created_seq : null,
      next_after: last && Number(last.created_seq) < total ? last.created_seq : null,
    };
  };
}

async function detach(chat: ChatStore, total: number): Promise<void> {
  const tail = await longHistory(total)(CONV, {});
  snapshot(chat, { history: tail });
  while (!chat.timeline.tailDetached) await chat.loadOlder();
}

describe("лента дальше 3000 записей", () => {
  it("пока хвост оторван, очередь, активность, исполнение и ввод применяются сразу", async () => {
    const chat = chatOf({ history: longHistory(3300) });
    await detach(chat, 3300);
    apply(
      chat,
      ev("derived_changed", { activity: { state: "working" }, queue: { queued: 1 } }, "4001"),
      ev("queue_state_changed", { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: "i1" } } }, "4002"),
      ev("execution_state_changed", { state: "running" }, "4003", "e1"),
      ev("input_queued", { input_id: "i2", input_seq: "4004", idempotency_key: "k2", state: "queued", content: [{ type: "text", text: "ещё" }] }, "4004"),
    );
    expect(chat.activity?.state).toBe("working");
    expect(chat.queue.queued).toBe(1);
    expect(chat.queue.run_slot).toEqual({ running: { input_id: "i1" } });
    expect(chat.execution?.state).toBe("running");
    expect(chat.inputs.get("k2")?.state).toBe("queued");
    expect(chat.strip().map((input) => input.key)).toEqual(["k2"]);
  });

  it("живой блок за краем окна копится в модели и считается новым, лента не меняется", async () => {
    const chat = chatOf({ history: longHistory(3300) });
    await detach(chat, 3300);
    const before = chat.timeline.entries;
    const id = "00000000-0000-4000-8000-00000000ffff";
    apply(
      chat,
      ev("block_started", { block_id: id, rev: 0, kind: "text", input_command_id: null, parent_block_id: null, agent: "a", model: "m", tool_name: null, summary: null, approved_by: null }, "4001"),
      ev("block_delta", { block_id: id, base_rev: 0, rev: 1, append: "живое" }, "4002"),
    );
    expect(chat.blocks.get(id)?.text).toBe("живое");
    expect(chat.timeline.entries).toBe(before);
    expect(chat.timeline.newerCount).toBe(1);
  });

  it("подгрузка вниз возвращает ленту к хвосту только на последней странице", async () => {
    const chat = chatOf({ history: longHistory(3300) });
    await detach(chat, 3300);
    for (let more = 0; more < 3; more += 1) await chat.loadOlder();
    let pages = 0;
    while (chat.timeline.tailDetached && pages < 100) {
      await chat.loadNewer();
      pages += 1;
    }
    expect(pages).toBeGreaterThan(1);
    expect(chat.timeline.tailDetached).toBe(false);
    expect(chat.timeline.newestSeq()).toBe("3300");
    expect(chat.timeline.entries.length).toBeLessThanOrEqual(3000);
    expect(chat.timeline.hasOlder).toBe(true);
  });

  it("кнопка к последним заменяет окно хвостом без разрыва", async () => {
    const chat = chatOf({ history: longHistory(3300) });
    await detach(chat, 3300);
    await chat.reloadTail();
    expect(chat.timeline.tailDetached).toBe(false);
    expect(chat.timeline.oldestSeq()).toBe("3201");
    expect(chat.timeline.newestSeq()).toBe("3300");
    expect(chat.timeline.hasOlder).toBe(true);
  });

  it("конец сжатия не уводит читающего старое к хвосту", async () => {
    const chat = chatOf({ history: longHistory(3300) });
    await detach(chat, 3300);
    const before = chat.timeline.entries;
    apply(chat, ev("compaction_finished", { id: "c1", ok: true }, "4001"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(chat.timeline.tailDetached).toBe(true);
    expect(chat.timeline.entries).toBe(before);
  });

  it("снимок при оторванном окне заменяет его хвостом без разрыва", async () => {
    const chat = chatOf({ history: longHistory(3300) });
    await detach(chat, 3300);
    snapshot(chat, { history: await longHistory(3300)(CONV, {}) });
    expect(chat.timeline.tailDetached).toBe(false);
    expect(chat.timeline.oldestSeq()).toBe("3201");
    expect(chat.timeline.hasOlder).toBe(true);
  });

  it("переполненное удержание сбрасывается и просит свежий снимок", async () => {
    let release: ((page: { items: HistoryItem[] }) => void) | undefined;
    let resyncs = 0;
    const chat = chatOf({ history: () => new Promise((resolve) => { release = resolve; }) }, () => { resyncs += 1; });
    chat.open();
    const pending = chat.reloadTail();
    const id = "00000000-0000-4000-8000-00000000ffff";
    apply(chat, ev("block_started", { block_id: id, rev: 0, kind: "text", input_command_id: null, parent_block_id: null, agent: "a", model: "m", tool_name: null, summary: null, approved_by: null }, "2"));
    for (let rev = 1; rev <= 5000; rev += 1)
      apply(chat, ev("block_delta", { block_id: id, base_rev: rev - 1, rev, append: "x" }, String(rev + 2)));
    release?.({ items: [] });
    await pending;
    expect(resyncs).toBe(1);
    expect(chat.blocks.get(id)).toBeUndefined();
  });
});

describe("хвост, который не стыкуется со старым окном", () => {
  const seqs = (chat: ChatStore): number[] => chat.timeline.entries.map((entry) => Number(entry.seq));
  const holes = (list: number[]): number =>
    list.length === 0 ? 0 : list.at(-1)! - list[0]! + 1 - list.length;

  it("снимок после долгого отсутствия заменяет окно хвостом, а не оставляет дыру", async () => {
    const chat = chatOf({ history: longHistory(400) });
    snapshot(chat, { history: await longHistory(100)(CONV, {}) });
    expect(seqs(chat)).toHaveLength(100);

    // Пока подписка была отпущена, агент написал больше страницы.
    snapshot(chat, { history: await longHistory(400)(CONV, {}) });
    expect(holes(seqs(chat))).toBe(0);
    expect(chat.timeline.oldestSeq()).toBe("301");
    expect(chat.timeline.newestSeq()).toBe("400");
    expect(chat.timeline.hasOlder).toBe(true);

    // Пропущенная середина догружается обычной страницей старых.
    await chat.loadOlder();
    expect(holes(seqs(chat))).toBe(0);
    expect(seqs(chat)).toContain(250);
  });

  it("перечитанный хвост при живом окне тоже не оставляет дыру", async () => {
    const chat = chatOf({ history: longHistory(400) });
    snapshot(chat, { history: await longHistory(100)(CONV, {}) });
    await chat.reloadTail();
    expect(holes(seqs(chat))).toBe(0);
    expect(chat.timeline.oldestSeq()).toBe("301");
    expect(chat.timeline.hasOlder).toBe(true);
  });

  it("хвост, который перекрывает окно, старые записи оставляет", async () => {
    const chat = chatOf({ history: longHistory(250) });
    snapshot(chat, { history: await longHistory(200)(CONV, {}) });
    await chat.loadOlder();
    expect(seqs(chat)).toHaveLength(200);

    snapshot(chat, { history: await longHistory(250)(CONV, {}) });
    expect(holes(seqs(chat))).toBe(0);
    expect(chat.timeline.oldestSeq()).toBe("1");
    expect(chat.timeline.newestSeq()).toBe("250");

    await chat.reloadTail();
    expect(chat.timeline.oldestSeq()).toBe("1");
    expect(seqs(chat)).toHaveLength(250);
  });

  it("короткая история целиком в снимке: окно не сбрасывается", async () => {
    const chat = chatOf();
    snapshot(chat, { history: await longHistory(40)(CONV, {}) });
    const version = chat.timeline.windowVersion;
    snapshot(chat, { history: await longHistory(60)(CONV, {}) });
    expect(seqs(chat)).toHaveLength(60);
    expect(chat.timeline.windowVersion).toBe(version);
  });
});

describe("финал блока за окном снимка", () => {
  const CALL = "00000000-0000-4000-8000-0000000000c1";
  const history = (state: string): HistoryItem => ({
    kind: "block",
    id: CALL,
    block_kind: "tool_call",
    tool_name: "Task",
    content: "{}",
    content_bytes: 2,
    content_truncated: false,
    created_seq: "10",
    created_at: "2026-10-28T12:00:00.000Z",
    rev: 3,
    state,
  });
  const final = ev("block_final", { block_id: CALL, rev: 3, text: "{}", text_omitted: false, checksum: "x", summary: null }, "900");

  it("старшая страница возвращает вызову субагента его вид", async () => {
    const chat = chatOf({ history: async () => ({ items: [history("final")], next_before: null }) });
    apply(chat, final);
    await chat.loadOlder();
    const block = chat.blocks.get(CALL)!;
    expect(block.kind).toBe("tool_call");
    expect(block.toolName).toBe("Task");
    const entry = chat.timeline.entries.find((item) => item.key === `block:${CALL}`);
    expect(entry?.kind === "block" && entry.block).toBe(block);
  });

  it("лента субагента тоже берёт вид из истории", async () => {
    const chat = chatOf({
      lane: async () => ({ root: { ...history("final"), kind: undefined } as never, items: [] }),
    });
    apply(chat, final);
    const page = await chat.loadLane(CALL);
    expect(page.entries[0]!.kind === "block" && page.entries[0]!.block.kind).toBe("tool_call");
  });
});

describe("отметка прочтения", () => {
  it("последняя отметка внутри паузы уходит, когда пауза кончится", () => {
    const reads: string[] = [];
    const clock = new FakeClock(1);
    const chat = chatOf({ markRead: async (_id, seq) => { reads.push(seq); } }, undefined, clock);
    chat.markRead("5");
    clock.advance(500);
    chat.markRead("6");
    chat.markRead("7");
    expect(reads).toEqual(["5"]);
    clock.advance(1500);
    expect(reads).toEqual(["5", "7"]);
    clock.advance(5000);
    expect(reads).toEqual(["5", "7"]);
  });
});

describe("правило автопинка", () => {
  const rule = (patch: Record<string, unknown> = {}) => ({
    session_id: CONV,
    enabled: true,
    state: "watching",
    idle_minutes: 3,
    background_minutes: 10,
    max_consecutive: 3,
    max_total: 20,
    done_markers: ["STATUS: DONE", "STATUS: BLOCKED"],
    context_warn_percent: 80,
    set_by: "agent",
    set_by_session: null,
    notify_session_id: null,
    consecutive: 0,
    total: 1,
    held: false,
    ...patch,
  });

  it("живое событие ставит правило, а снимок после переподключения его перечитывает", () => {
    const chat = chatOf();
    apply(chat, ev("nudge_changed", { ...rule(), detail: "held" }, "2"));
    expect(chat.nudge?.state).toBe("watching");

    snapshot(chat, {
      nudge: rule({ state: "pending", idle_minutes: 6 }),
      nudge_events: [
        {
          id: "00000000-0000-4000-8000-0000000000e1",
          at: "2026-11-05T12:00:00.000Z",
          attempt: 1,
          outcome: "nudged",
          signals: { no_marker: true },
        },
      ],
    });
    expect(chat.nudge?.state).toBe("pending");
    expect(chat.nudge?.idle_minutes).toBe(6);
    expect(chat.nudgeEvents).toHaveLength(1);

    apply(chat, ev("nudge_changed", { ...rule(), detail: "cleared" }, "3"));
    expect(chat.nudge).toBeNull();
    expect(chat.nudgeEvents).toHaveLength(1);
  });
});

describe("prompt suggestion", () => {
  it("comes from the snapshot, follows the event and is cleared by null", () => {
    const chat = chatOf();
    snapshot(chat, { prompt_suggestion: { text: "добавь тесты" } });
    expect(chat.promptSuggestion).toBe("добавь тесты");
    apply(chat, ev("prompt_suggestion_changed", {
      conversation_id: CONV,
      suggestion: { text: "запусти сборку", execution_id: "00000000-0000-4000-8000-0000000000e1", created_at: "2026-10-28T12:00:00.000Z" },
    }, "2"));
    expect(chat.promptSuggestion).toBe("запусти сборку");
    apply(chat, ev("prompt_suggestion_changed", { conversation_id: CONV, suggestion: null }, "3"));
    expect(chat.promptSuggestion).toBeNull();
    snapshot(chat, { prompt_suggestion: { text: "снова" } });
    snapshot(chat, {});
    expect(chat.promptSuggestion).toBeNull();
  });

  it("never reaches the shared draft", () => {
    const chat = chatOf();
    snapshot(chat, { prompt_suggestion: { text: "добавь тесты" } });
    expect(chat.draft).toBeNull();
  });
});
