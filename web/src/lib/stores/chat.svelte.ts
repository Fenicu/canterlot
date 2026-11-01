import { untrack } from "svelte";
import { SvelteMap, SvelteSet } from "svelte/reactivity";
import type { OpJournal } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import type {
  AppliedEvent,
  RangeResult,
  ScopeApplier,
  ScopeStatus,
} from "#lib/sync/scope.js";
import type { HubEvent, ScopeName } from "#lib/sync/wire.js";
import {
  BlockModel,
  BlockSync,
  applyBlockEvent,
  type BlockIO,
  type BlockKind,
  type BlockPage,
} from "./blocks";
import { utf8Length } from "./blocks";
import {
  assignFeedSeq,
  blankAttachment,
  freshInput,
  inputOfBody,
  marksFor,
  placeInput,
  type AttachmentRef,
  type InputUiState,
  type InputView,
} from "./inputs";
import { LOCAL_SEQ, TimelineModel, cmpSeq, type TimelineEntry } from "./timeline";
import { rawCell } from "./cell.svelte.js";
import { RequestsModel, isDocked } from "./requests.svelte";
import { BackgroundModel, type BackgroundTask } from "./background.svelte";
import { TodosModel, type TodoList } from "./todos.svelte";
import type { components } from "#lib/api/generated/openapi.js";

export interface HistoryBlock {
  kind: "block";
  id: string;
  block_kind: BlockKind;
  content: string;
  content_bytes: number;
  content_truncated: boolean;
  created_seq: string;
  created_at: string;
  rev: number;
  state: string;
  parent_block_id?: string | null;
  agent?: string;
  model?: string;
  tool_name?: string | null;
  summary?: string | null;
  approved_by?: string | null;
  input_command_id?: string | null;
  incomplete_reason?: string | null;
  line_count?: number | null;
  started_at?: string | null;
  finished_at?: string | null;
  media?: {
    media_id: string;
    mime: string;
    width?: number | null;
    height?: number | null;
    ready?: boolean;
  }[];
}

export interface HistoryInput {
  kind: "input";
  id: string;
  idempotency_key: string;
  state: string;
  content: {
    type: string;
    text?: string;
    attachment_id?: string;
    name?: string;
    mime?: string;
    size?: number;
    sha256?: string;
    image?: boolean;
  }[];
  attachments: AttachmentRef[];
  created_seq: string;
  feed_seq?: string | null;
  created_at: string;
  retry_of?: string | null;
}

export type HistoryRequest = components["schemas"]["RequestItemView"] & {
  kind: "request";
};

export type HistoryItem = HistoryBlock | HistoryInput | HistoryRequest;

export interface HistoryPage {
  items: HistoryItem[];
  next_before?: string | null;
  next_after?: string | null;
}

/** Страница ленты одного субагента: корень и потомки по возрастанию номера, без корня. */
export interface LanePage {
  root: Omit<HistoryBlock, "kind">;
  items: Omit<HistoryBlock, "kind">[];
  next_before?: string | null;
}

export interface ChatApi {
  conversationSnapshot(
    id: string,
  ): Promise<{ cursor: string; data: SnapshotData }>;
  history(
    id: string,
    query?: { before?: string; after?: string; limit?: number },
  ): Promise<HistoryPage>;
  /** Страница ленты субагента; без `before` отдаёт самых новых потомков. */
  lane?(
    id: string,
    blockId: string,
    query?: { before?: string; limit?: number },
  ): Promise<LanePage>;
  events(scope: string, after: string, limit: number): Promise<RangeResult>;
  markRead(id: string, seq: string): Promise<void>;
}

type QueueSnapshot = components["schemas"]["SnapshotQueueView"];
/** Снимок даёт всё исполнение, событие смены состояния только часть полей. */
export type ChatExecution = Partial<components["schemas"]["LiveExecutionView"]> & {
  last_run?: unknown;
};

const LIVE_EXECUTION_STATES = ["created", "starting", "running", "stopping"];

export function executionLive(execution: ChatExecution | null): boolean {
  return LIVE_EXECUTION_STATES.includes(execution?.state ?? "");
}
type QueuedInput = QueueSnapshot["queued"][number];
export type ScheduledMessage = components["schemas"]["ScheduledMessage"];

export interface SnapshotData {
  detail?: Record<string, unknown> | null;
  queue?: Partial<QueueSnapshot>;
  history?: HistoryPage;
  scheduled?: ScheduledMessage[];
  open_requests?: unknown[];
  todos?: TodoList[];
  background?: BackgroundTask[];
}

export type RunSlot =
  | "free"
  | { dispatching: { input_id: string; attempt: number } }
  | { running: { input_id: string } };

type Clock = {
  now(): number;
  schedule?: (fn: () => void, ms: number) => () => void;
};

const SERVER_STATES = new Set<InputUiState>([
  "queued",
  "dispatched",
  "on_machine",
  "transmitting",
  "accepted",
  "acceptance_unconfirmed",
  "cancelled",
  "rejected",
]);

function asState(value: string): InputUiState {
  return SERVER_STATES.has(value as InputUiState)
    ? (value as InputUiState)
    : "queued";
}

/** Состояния, в которых сообщение уже отправлено, но ход по нему ещё не принят. */
const ON_ITS_WAY: ReadonlySet<InputUiState> = new Set([
  "saving",
  "queued",
  "dispatched",
  "on_machine",
  "transmitting",
]);
/** Эти удержания принадлежат самому сообщению и не делают его ждущим чужого хода. */
const OWN_HOLDS: ReadonlySet<string> = new Set(["attachments_pending", "run_in_progress"]);

function reasonOf(hold: unknown): string {
  return typeof hold === "object" && hold !== null
    ? String((hold as { reason?: unknown }).reason ?? "")
    : "";
}

function textOf(content: HistoryInput["content"]): string {
  return content
    .filter((block) => block.type === "text")
    .map((block) => block.text ?? "")
    .join("");
}

const MAX_DELTA_MARKS = 512;
const MAX_DIAGNOSTICS = 200;

function scopeOf(id: string): ScopeName {
  return `conversation:${id}`;
}

/** Записи полосы меняются на месте (состояние, место, отметки): без прокси SvelteMap об этом не узнает. */
function reactiveInput(value: InputView): InputView {
  const view = $state(value);
  return view;
}

class InputMap extends SvelteMap<string, InputView> {
  override set(key: string, value: InputView): this {
    return super.set(key, reactiveInput(value));
  }
}

export class ChatStore {
  readonly blocks = new SvelteMap<string, BlockModel>();
  readonly inputs = new InputMap();
  readonly timeline = new TimelineModel();
  readonly requests: RequestsModel;
  readonly todos = new TodosModel();
  readonly background = new BackgroundModel();
  readonly diagnostics: unknown[] = [];
  readonly queue = $state({
    pause_reasons: [] as unknown[],
    hold_reasons: [] as unknown[],
    run_slot: "free" as RunSlot,
    queued: 0,
  });
  detail = $state<Record<string, unknown> | null>(null);
  /** Записан принятый ход: снимок даёт хаб, новый ход виден по событию. */
  turnRecorded = $state(false);
  /** Сессию удалили, пока чат был открыт: подписка уже закрыта. */
  deleted = $state(false);
  execution = $state<ChatExecution | null>(null);
  /** Исполнения ещё не знаем сразу после снимка: строки снимка хаб уже привёл к его состоянию. */
  readonly backgroundFacts = $derived(
    this.background.facts(this.execution === null || executionLive(this.execution)),
  );
  activity = $state<Record<string, unknown> | null>(null);
  settings = $state<{
    effective: unknown;
    pending: unknown;
    rev: number;
    rejected?: { keys: string[]; reason: string } | null;
  }>({
    effective: null,
    pending: null,
    rev: 0,
    rejected: null,
  });
  crunch = $state<{
    enabled: boolean;
    resume_pending: boolean;
    resume_text?: string | null;
    reserve_override_until?: string | null;
  }>({
    enabled: false,
    resume_pending: false,
  });
  scheduled = $state<ScheduledMessage[]>([]);
  phase = $state<Record<string, unknown> | null>(null);
  interrupt = $state<Record<string, unknown> | null>(null);
  attachments = new SvelteMap<string, Record<string, unknown>>();
  unknownKinds = $state(0);
  hiddenCount = $state(0);
  queuePausedCount = $state(0);
  following = true;
  deltaMarks: string[] = [];
  deltaSerial = $state(0);
  /** Счётчик отправок с этого устройства: лента по нему возвращается к хвосту. */
  ownSends = $state(0);


  readonly #sync: BlockSync;
  #sub: {
    release(): void;
    readonly status: ScopeStatus;
    readonly cursor: string;
  } | null = null;
  #hold = false;
  #held: HubEvent[] = [];
  #lanes = new SvelteMap<string, number>();
  #laneLive = rawCell<TimelineEntry[]>([]);
  #readSeq = 0n;
  #readAt = -10_000;
  #noteTimers = new SvelteMap<string, () => void>();
  #applier: ScopeApplier | null = null;
  #stopRefused: (() => void) | null = null;

  constructor(
    readonly id: string,
    private deps: {
      api: ChatApi;
      ops: OpJournal;
      drafts: DraftStore;
      connection: ConnectionManager;
      blockIO: BlockIO;
      clock: Clock;
      probe: unknown;
    },
  ) {
    this.#sync = new BlockSync(this.blocks, deps.blockIO, {
      schedule: deps.clock.schedule,
    });
    this.requests = new RequestsModel(this.id);
  }

  get status(): ScopeStatus {
    return this.#sub?.status ?? "idle";
  }

  get cursor(): string | null {
    return this.#sub?.cursor ?? null;
  }

  /** Экран, который заменит чат, когда открывать больше нечего. */
  get gone(): "deleted" | "unavailable" | null {
    if (this.deleted || this.status === "deleted") return "deleted";
    return this.status === "forbidden" ? "unavailable" : null;
  }

  markDeleted(): void {
    this.deleted = true;
    this.close();
  }

  get applier(): ScopeApplier {
    // Один получатель на хранилище: по нему соединение отличает своё отпущенное окно от чужого.
    return (this.#applier ??= {
      loadSnapshot: async () => {
        const snap = await this.deps.api.conversationSnapshot(this.id);
        return { cursor: snap.cursor, data: snap.data };
      },
      applySnapshot: (data) => {
        this.#applySnapshot(data as SnapshotData);
      },
      applyEvents: (batch) => this.#applyBatch(batch),
      fetchRange: (after, limit) =>
        this.deps.api.events(scopeOf(this.id), after, limit),
    });
  }

  get ops(): OpJournal {
    return this.deps.ops;
  }

  open(): void {
    // Тёплый чат открывается прямо из $effect маршрута: без untrack экран пересоздавался бы при каждой операции.
    untrack(() => {
      this.#adoptSaving();
      this.#reviveNotes();
    });
    this.#stopRefused ??= this.deps.ops.onOrphanRefused((op) => {
      this.discardLocal(op.key);
    });
    if (this.#sub) return;
    this.#sub = this.deps.connection.subscribe(scopeOf(this.id), this.applier);
  }

  close(): void {
    this.#sub?.release();
    this.#sub = null;
    for (const cancel of this.#noteTimers.values()) cancel();
    this.#noteTimers.clear();
  }

  dispose(): void {
    this.close();
    this.#stopRefused?.();
    this.#stopRefused = null;
    this.#sync.stop();
    this.blocks.clear();
    this.inputs.clear();
    this.timeline.clear();
    this.attachments.clear();
    this.requests.items.clear();
    this.todos.clear();
    this.background.clear();
    this.diagnostics.length = 0;
    this.#held = [];
    this.#laneLive.set([]);
    this.detail = null;
    this.execution = null;
    this.activity = null;
    this.scheduled = [];
    this.settings = { effective: null, pending: null, rev: 0 };
    this.crunch = { enabled: false, resume_pending: false };
  }

  async loadOlder(): Promise<void> {
    const page = await this.deps.api.history(this.id, {
      before: this.timeline.oldestSeq() ?? undefined,
    });
    this.#ingest(page.items, "older");
    this.timeline.hasOlder = Boolean(page.next_before);
  }

  /**
   * Одна страница ленты субагента. Блоки попадают в общую карту, как при чтении истории,
   * но лента разговора и её окно не меняются; записи возвращаются для ленты субагента.
   */
  async loadLane(
    rootId: string,
    before?: string,
  ): Promise<{ entries: TimelineEntry[]; nextBefore: string | null }> {
    const fetchLane = this.deps.api.lane;
    if (!fetchLane) throw new Error("lane_unavailable");
    const page = await fetchLane.call(this.deps.api, this.id, rootId, { before });
    const entries = [page.root, ...page.items].map((item) => {
      const known = this.blocks.get(item.id);
      // Живое событие могло обогнать ответ: более новую ревизию не затирает снимок.
      const block =
        known && known.rev >= item.rev && known.state !== "stale"
          ? known
          : this.#blockFromHistory({ ...item, kind: "block" }, false);
      return {
        kind: "block" as const,
        key: `block:${block.id}`,
        seq: block.createdSeq || item.created_seq,
        block,
      };
    });
    return { entries, nextBefore: page.next_before ?? null };
  }

  async loadNewer(): Promise<void> {
    this.#hold = true;
    try {
      const page = await this.deps.api.history(this.id, {
        after: this.timeline.newestSeq() ?? undefined,
      });
      this.#ingest(page.items, "newer");
      this.timeline.tailDetached = false;
      this.timeline.newerCount = 0;
    } finally {
      this.#hold = false;
      this.#flushHeld();
    }
  }

  async reloadTail(): Promise<void> {
    this.#hold = true;
    try {
      const page = await this.deps.api.history(this.id, {});
      this.#ingest(page.items, "tail");
    } finally {
      this.#hold = false;
      this.#flushHeld();
    }
  }

  markRead(createdSeq: string): void {
    if (!this.following || !visible()) return;
    let seq: bigint;
    try {
      seq = BigInt(createdSeq);
    } catch {
      return;
    }
    if (seq <= this.#readSeq) return;
    const now = this.deps.clock.now();
    if (now - this.#readAt < 2000) return;
    this.#readSeq = seq;
    this.#readAt = now;
    void this.deps.api.markRead(this.id, createdSeq);
  }

  async blockContent(blockId: string): Promise<AsyncIterable<BlockPage>> {
    const io = this.deps.blockIO;
    return (async function* () {
      let offset = 0;
      for (;;) {
        const page = await io.fetchContent(blockId, offset);
        yield page;
        const next = page.next_offset ?? page.offset + utf8Length(page.content);
        if (next <= offset || next >= page.total_bytes) break;
        offset = next;
      }
    })();
  }

  strip(): InputView[] {
    return [...this.inputs.values()].filter(
      (input) =>
        input.placement === "strip" ||
        (input.state === "cancelled" && input.stripUntil !== null),
    );
  }

  /**
   * Новое сообщение владельца. Если ничто не мешает уйти сразу, пузырь входит в ленту с первого
   * кадра; ожидающее за ходом, паузой или другим сообщением остаётся плашкой над полем ввода.
   */
  admitLocal(view: InputView): void {
    const free = this.#mayGoNow();
    this.inputs.set(view.key, view);
    this.ownSends += 1;
    if (!free) return;
    const input = this.inputs.get(view.key)!;
    input.optimistic = true;
    input.placement = "feed";
    this.#seat(input, LOCAL_SEQ);
  }

  /** Сообщение, которое не ушло и возвращается автору: пузырь исчезает вместе с записью. */
  discardLocal(key: string): void {
    this.inputs.delete(key);
    this.timeline.remove(`input:${key}`);
  }

  #mayGoNow(): boolean {
    if (this.timeline.tailDetached) return false;
    if (this.activity?.state !== "idle") return false;
    if (this.queue.queued > 0) return false;
    if (this.queue.pause_reasons.length > 0 || this.queue.hold_reasons.length > 0)
      return false;
    if (this.queue.run_slot && this.queue.run_slot !== "free") return false;
    return ![...this.inputs.values()].some((input) => ON_ITS_WAY.has(input.state));
  }

  /** Причина, по которой отправленное сообщение уже не уходит само: чужой ход или пауза. */
  #mustWait(input: InputView): boolean {
    const slot = this.queue.run_slot;
    const other =
      slot && slot !== "free"
        ? "running" in slot
          ? slot.running.input_id
          : slot.dispatching.input_id
        : null;
    return (
      this.queue.pause_reasons.length > 0 ||
      this.queue.hold_reasons.some((hold) => !OWN_HOLDS.has(reasonOf(hold))) ||
      (other !== null && other !== input.id)
    );
  }

  #seat(input: InputView, seq: string): void {
    this.timeline.upsert({
      kind: "input",
      key: `input:${input.key}`,
      seq,
      input,
    });
  }

  #demote(input: InputView): void {
    input.optimistic = false;
    input.placement = "strip";
    this.timeline.remove(`input:${input.key}`);
  }

  /** Пауза, пришедшая после отправки: отменить ввод можно только из полосы. */
  #demoteWaiting(): void {
    for (const input of this.inputs.values()) {
      if (input.optimistic && placeInput(input.state) === "strip" && this.#mustWait(input))
        this.#demote(input);
    }
  }

  #applyBatch(batch: AppliedEvent[]): void {
    for (const item of batch) this.#take(item.event);
  }

  #take(event: HubEvent): void {
    if (event.kind === "hidden") {
      this.hiddenCount += 1;
      return;
    }
    // Лента держит события, пока читают старое; панели задач ждать нечего, порядок даёт ревизия.
    if (event.kind === "todo_updated") {
      this.todos.apply(event.payload);
      return;
    }
    if (event.kind === "background_work_changed") {
      this.background.apply(event.payload.tasks);
      return;
    }
    if (this.#hold || this.timeline.tailDetached) {
      // Открытая лента субагента живёт независимо от прокрутки ленты разговора.
      if (!this.#hold && this.#inOpenLane(event)) {
        this.#dispatch(event);
        return;
      }
      this.#held.push(event);
      if (
        this.timeline.tailDetached &&
        (event.kind === "block_started" || event.kind === "input_queued")
      ) {
        this.timeline.newerCount += 1;
      }
      return;
    }
    this.#dispatch(event);
  }

  /** Лента субагента открыта: её блоки применяются сразу, пока лента разговора оторвана от хвоста. */
  watchLane(rootId: string): () => void {
    this.#lanes.set(rootId, (this.#lanes.get(rootId) ?? 0) + 1);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      const left = (this.#lanes.get(rootId) ?? 1) - 1;
      if (left > 0) this.#lanes.set(rootId, left);
      else this.#lanes.delete(rootId);
      if (this.#lanes.size === 0) this.#laneLive.set([]);
    };
  }

  /** Записи блоков открытых лент, пришедших живыми, пока лента разговора оторвана от хвоста. */
  get laneLive(): TimelineEntry[] {
    return this.#laneLive.get();
  }

  #inOpenLane(event: HubEvent): boolean {
    if (this.#lanes.size === 0) return false;
    if (event.kind === "block_started")
      return this.#underOpenLane(event.payload.parent_block_id);
    if (
      event.kind === "block_delta" ||
      event.kind === "block_final" ||
      event.kind === "block_incomplete"
    )
      return this.#underOpenLane(event.payload.block_id);
    return false;
  }

  #underOpenLane(start: string | null): boolean {
    let id = start;
    for (let depth = 0; id && depth < 64; depth += 1) {
      if (this.#lanes.has(id)) return true;
      id = this.blocks.get(id)?.parentId ?? null;
    }
    return false;
  }

  #flushHeld(): void {
    const events = this.#held.splice(0);
    for (const event of events) this.#dispatch(event);
  }


  #dispatch(event: HubEvent): void {
    const kind = event.kind as string;
    switch (kind) {
      case "hidden":
        this.hiddenCount += 1;
        return;
      case "input_queued":
        this.#queued(event);
        return;
      case "input_state_changed":
        this.#inputState(event);
        return;
      case "block_started":
      case "block_delta":
      case "block_final":
      case "block_incomplete":
        this.#block(event);
        return;
      case "block_media_attached":
        if (event.kind === "block_media_attached")
          this.#attachMedia(event.payload);
        return;
      case "run_finished":
        if (event.kind === "run_finished")
          this.execution = {
            ...(this.execution ?? {}),
            last_run: event.payload,
          };
        return;
      case "phase_changed":
        if (event.kind === "phase_changed") this.phase = { ...event.payload };
        return;
      case "queue_state_changed":
        if (event.kind === "queue_state_changed") {
          this.queue.pause_reasons = event.payload.pause_reasons;
          this.queue.hold_reasons = event.payload.hold_reasons;
          this.queue.run_slot = event.payload.run_slot;
          this.#demoteWaiting();
        }
        return;
      case "queue_paused":
        this.queuePausedCount += 1;
        return;
      case "queue_resumed":
        if (event.kind === "queue_resumed") {
          this.queue.pause_reasons = event.payload.pause_reasons;
          this.#demoteWaiting();
        }
        return;
      case "execution_state_changed":
        if (event.kind === "execution_state_changed")
          this.execution = {
            ...this.execution,
            ...event.payload,
            execution_id: event.execution_id ?? this.execution?.execution_id,
          };
        return;
      case "conversation_settings_changed":
        if (event.kind === "conversation_settings_changed") {
          this.settings = {
            effective: event.payload.effective,
            pending: event.payload.pending,
            rev: event.payload.rev,
            rejected: event.payload.rejected ?? null,
          };
        }
        return;
      case "conversation_updated":
        if (event.kind === "conversation_updated")
          this.detail = { ...(this.detail ?? {}), ...event.payload };
        return;
      case "crunch_changed":
        if (event.kind === "crunch_changed") {
          this.crunch = {
            enabled: event.payload.enabled,
            resume_text: event.payload.resume_text,
            reserve_override_until: event.payload.reserve_override_until,
            resume_pending: event.payload.resume_pending,
          };
        }
        return;
      case "crunch_held":
        if (event.kind === "crunch_held")
          this.crunch = {
            ...this.crunch,
            resume_pending: event.payload.resume_pending,
          };
        return;
      case "crunch_woken":
        if (event.kind === "crunch_woken")
          this.crunch = {
            ...this.crunch,
            resume_pending: event.payload.resume_pending,
          };
        return;
      case "limit_reached":
        if (event.kind === "limit_reached") this.#diagnose(event);
        return;
      case "scheduled_changed":
        if (event.kind === "scheduled_changed") {
          const next = this.scheduled.filter(
            (item) => item.id !== event.payload.id,
          );
          next.push(event.payload as ScheduledMessage);
          this.scheduled = next;
        }
        return;
      case "attachment_state_changed":
        if (event.kind === "attachment_state_changed") {
          const prev = this.attachments.get(event.payload.attachment_id) ?? {};
          const next: Record<string, unknown> = {
            ...prev,
            ...(event.payload as unknown as Record<string, unknown>),
          };
          this.attachments.set(event.payload.attachment_id, next);
          for (const input of this.inputs.values()) {
            for (let i = 0; i < input.attachments.length; i++) {
              if (
                input.attachments[i]?.attachment_id ===
                event.payload.attachment_id
              ) {
                const current = input.attachments[i]!;
                input.attachments[i] = {
                  attachment_id: event.payload.attachment_id,
                  name:
                    typeof next.name === "string" ? next.name : current.name,
                  mime:
                    typeof next.mime === "string" ? next.mime : current.mime,
                  size:
                    typeof next.size === "number" ? next.size : current.size,
                  sha256:
                    typeof next.sha256 === "string"
                      ? next.sha256
                      : current.sha256,
                  image:
                    typeof next.image === "boolean"
                      ? next.image
                      : current.image,
                };
              }
            }
          }
        }
        return;
      case "derived_changed":
        if (event.kind === "derived_changed") {
          this.activity = { ...event.payload.activity };
          this.queue.queued = event.payload.queue.queued;
        }
        return;
      case "interrupt_state_changed":
        if (event.kind === "interrupt_state_changed") {
          this.interrupt = { ...event.payload };
          if (
            event.payload.status === "completed" ||
            event.payload.status === "acknowledged"
          ) {
            this.requests.applyInterrupt(event);
          }
        }
        return;
      case "request_opened":
        this.requests.applyOpened(event);
        this.#placeRequest(event);
        return;
      case "request_closed":
        this.requests.applyClosed(event);
        return;
      case "request_decided":
        this.requests.applyDecided(event);
        return;
      case "request_transmitting":
        this.requests.applyTransmitting(event);
        return;
      case "block_conflicting_final":
      case "adapter_diagnostic":
      case "state_divergence":
      case "block_revision_gap":
      case "block_checksum_mismatch":
        this.#diagnose(event);
        return;
      default:
        this.unknownKinds += 1;
    }
  }

  /** Место в ленте запрос получает сразу: туда он и перейдёт, когда владелец ответит. */
  #placeRequest(event: HubEvent): void {
    if (event.kind !== "request_opened") return;
    const request = this.requests.items.get(event.payload.request_id);
    if (!request) return;
    this.timeline.upsert({
      kind: "request",
      key: `request:${request.id}`,
      seq: event.scope_seq,
      request,
    });
  }

  #diagnose(item: unknown): void {
    this.diagnostics.push(item);
    if (this.diagnostics.length > MAX_DIAGNOSTICS)
      this.diagnostics.splice(0, this.diagnostics.length - MAX_DIAGNOSTICS);
  }

  #attachMedia(payload: {
    block_id: string;
    media_id: string;
    mime: string;
    width: number | null;
    height: number | null;
  }): void {
    const block = this.blocks.get(payload.block_id);
    if (!block) return;
    const next = block.media.map((item) => ({ ...item }));
    const found = next.find((item) => item.media_id === payload.media_id);
    const ready = {
      media_id: payload.media_id,
      mime: payload.mime,
      width: payload.width ?? 1,
      height: payload.height ?? 1,
      ready: true,
    };
    if (found) Object.assign(found, ready);
    else next.push(ready);
    block.media = next;
  }

  #block(event: HubEvent): void {
    const effects = applyBlockEvent(this.blocks, event);
    this.#sync.handle(effects);
    for (const effect of effects)
      if (effect.type === "diag") this.#diagnose(effect);
    if (event.kind === "block_delta") {
      this.deltaMarks.push(event.committed_at_from);
      if (this.deltaMarks.length > MAX_DELTA_MARKS)
        this.deltaMarks.splice(0, this.deltaMarks.length - MAX_DELTA_MARKS);
      this.deltaSerial += 1;
    }
    if (event.kind === "block_started") {
      const block = this.blocks.get(event.payload.block_id);
      if (block) {
        const entry: TimelineEntry = {
          kind: "block",
          key: `block:${block.id}`,
          seq: block.createdSeq,
          block,
        };
        this.timeline.upsert(entry);
        if (
          this.timeline.tailDetached &&
          this.#underOpenLane(event.payload.parent_block_id)
        )
          this.#laneLive.set([...this.#laneLive.get(), entry]);
      }
    }
  }

  #queued(event: HubEvent): void {
    if (event.kind !== "input_queued") return;
    const payload = event.payload;
    const existing =
      this.inputs.get(payload.idempotency_key) ??
      freshInput(payload.idempotency_key, "", this.deps.clock.now());
    existing.id = payload.input_id;
    existing.text = textOf(payload.content as HistoryInput["content"]);
    const rawContent =
      (payload.content as Array<Partial<AttachmentRef> & { type: string }> | undefined) ?? [];
    const attBlocks = rawContent.filter(
      (b) => b.type === "attachment" && typeof b.attachment_id === "string",
    );
    if (attBlocks.length > 0) {
      existing.attachments = attBlocks.map((b) => {
        const id = b.attachment_id!;
        const registered = this.attachments.get(id) ?? {};
        const prior = existing.attachments.find((a) => a.attachment_id === id);
        // Событие готовности вложения может не нести имени: известное раньше не затирается пустым.
        const text = (key: "name" | "mime" | "sha256"): string => {
          const known = [registered[key], prior?.[key], b[key]].find(
            (value) => typeof value === "string" && value !== "",
          );
          return typeof known === "string" ? known : "";
        };
        const size = [registered.size, prior?.size, b.size].find(
          (value) => typeof value === "number" && value > 0,
        );
        return {
          attachment_id: id,
          name: text("name"),
          mime: text("mime"),
          size: typeof size === "number" ? size : 0,
          sha256: text("sha256"),
          image: Boolean(registered.image ?? prior?.image ?? b.image),
        };
      });
    }
    existing.state = "queued";
    existing.marks = marksFor("queued");
    existing.localOnly = false;
    existing.createdSeq = payload.input_seq;
    this.inputs.set(existing.key, existing);
    const seated = this.inputs.get(existing.key)!;
    if (seated.optimistic && this.#mustWait(seated)) this.#demote(seated);
    else if (!seated.optimistic) {
      seated.placement = placeInput("queued");
      this.timeline.remove(`input:${seated.key}`);
    }
    this.deps.ops.settle(existing.key);
  }

  #inputState(event: HubEvent): void {
    if (event.kind !== "input_state_changed") return;
    const payload = event.payload;
    const input = [...this.inputs.values()].find(
      (item) => item.id === payload.input_id,
    );
    if (!input) return;
    const state = asState(payload.state);
    input.state = state;
    input.marks = marksFor(state);
    if (state === "accepted") this.turnRecorded = true;
    input.attempt = payload.attempt ?? input.attempt;
    if (payload.retry_of) input.retryOf = payload.retry_of;
    if (payload.reason) input.rejection = payload.reason;
    if (payload.cause === "cancel_requested") input.cancelRequested = true;
    assignFeedSeq(input, state, event.scope_seq);
    if (state === "cancelled") {
      input.optimistic = false;
      input.placement = "strip";
      input.stripUntil = this.deps.clock.now() + 4000;
      this.timeline.remove(`input:${input.key}`);
      this.#armNote(input.key);
      return;
    }
    input.stripUntil = null;
    if (input.optimistic && placeInput(state) === "strip") {
      if (this.#mustWait(input)) this.#demote(input);
      return;
    }
    input.placement = placeInput(state);
    if (input.placement === "feed") input.optimistic = false;
    if (input.placement === "feed" && input.feedSeq) {
      this.timeline.upsert({
        kind: "input",
        key: `input:${input.key}`,
        seq: input.feedSeq,
        input,
      });
    } else this.timeline.remove(`input:${input.key}`);
  }

  /** close снимает таймеры заметок: при возврате просроченные убираются, остальные дожидаются срока. */
  #reviveNotes(): void {
    for (const input of [...this.inputs.values()]) {
      if (input.state !== "cancelled" || input.stripUntil === null) continue;
      const left = input.stripUntil - this.deps.clock.now();
      if (left > 0) this.#armNote(input.key, left);
      else this.inputs.delete(input.key);
    }
  }

  #armNote(key: string, ms = 4000): void {
    this.#noteTimers.get(key)?.();
    const drop = (): void => {
      this.#noteTimers.delete(key);
      const input = this.inputs.get(key);
      if (!input || input.state !== "cancelled") return;
      input.stripUntil = null;
      input.placement = "none";
      this.inputs.delete(key);
    };
    if (this.deps.clock.schedule)
      this.#noteTimers.set(key, this.deps.clock.schedule(drop, ms));
    else {
      const timer = setTimeout(drop, ms);
      this.#noteTimers.set(key, () => clearTimeout(timer));
    }
  }


  #applySnapshot(data: SnapshotData): void {
    if (data.detail) {
      this.detail = data.detail;
      this.turnRecorded = data.detail.turn_recorded === true;
      if (data.detail.execution)
        this.execution = data.detail.execution as ChatExecution;
      if (data.detail.activity)
        this.activity = data.detail.activity as Record<string, unknown>;
      if (data.detail.settings) {
        const s = data.detail.settings as {
          effective?: unknown;
          pending?: unknown;
          rev?: number;
        };
        this.settings = {
          effective: s.effective ?? null,
          pending: s.pending ?? null,
          rev: s.rev ?? 0,
          rejected: null,
        };
      }
      if (data.detail.crunch) {
        const c = data.detail.crunch as {
          enabled?: boolean;
          resume_pending?: boolean;
          resume_text?: string | null;
          reserve_override_until?: string | null;
        };
        this.crunch = {
          enabled: Boolean(c.enabled),
          resume_pending: Boolean(c.resume_pending),
          resume_text: c.resume_text,
          reserve_override_until: c.reserve_override_until,
        };
      }
    }
    if (data.queue) {
      this.queue.pause_reasons = data.queue.pause_reasons ?? [];
      this.queue.hold_reasons = data.queue.hold_reasons ?? [];
      if (data.queue.run_slot) this.queue.run_slot = data.queue.run_slot;
    }
    if (data.scheduled) this.scheduled = data.scheduled;
    if (data.open_requests) this.requests.ingestSnapshot(data.open_requests);
    if (data.todos) this.todos.ingestSnapshot(data.todos);
    if (data.background) this.background.ingestSnapshot(data.background);
    if (data.history)
      this.timeline.hasOlder = Boolean(data.history.next_before);
    const items = data.history?.items ?? [];
    const oldest = [...items].sort((a, b) => cmpSeq(seqOf(a), seqOf(b)))[0];
    const floor = oldest ? seqOf(oldest) : null;
    const older = floor
      ? this.timeline.entries.filter(
          (entry) => BigInt(entry.seq) < BigInt(floor),
        )
      : [];
    const keepOlder = older.every((entry) => terminalEntry(entry));
    if (!keepOlder) this.timeline.applyHistoryPage([], "tail");
    this.#ingest(items, "tail");
    if (!keepOlder) {
      const keptSeq = new SvelteSet(
        items.map((item) =>
          item.kind === "input"
            ? `input:${item.idempotency_key}`
            : `${item.kind}:${item.id}`,
        ),
      );
      for (const entry of older)
        if (!keptSeq.has(entry.key) && !terminalEntry(entry))
          this.timeline.remove(entry.key);
    }
    if (data.queue?.queued) this.#adoptQueued(data.queue.queued);
  }

  /** Вводы без места в ленте есть только в очереди снимка: полоса сходится с ней в обе стороны. */
  #adoptQueued(queued: QueuedInput[]): void {
    this.queue.queued = queued.length;
    const held = new SvelteSet(
      queued.map((view) => this.#inputFromHistory({ ...view, kind: "input" }).key),
    );
    for (const input of [...this.inputs.values()]) {
      const waiting = input.placement === "strip" || (input.optimistic && input.state === "queued");
      if (input.id && waiting && input.state !== "cancelled" && !held.has(input.key)) {
        this.inputs.delete(input.key);
        this.timeline.remove(`input:${input.key}`);
      }
    }
  }

  #ingest(items: HistoryItem[], dir: "older" | "newer" | "tail"): void {
    const entries: TimelineEntry[] = [];
    for (const item of items) {
      if (item.kind === "request") {
        entries.push({
          kind: "request",
          key: `request:${item.id}`,
          seq: item.created_seq,
          request: this.requests.ingestHistory(item),
        });
      } else if (item.kind === "block") {
        const block = this.#blockFromHistory(item, dir === "tail");
        entries.push({
          kind: "block",
          key: `block:${block.id}`,
          seq: block.createdSeq,
          block,
        });
      } else {
        const input = this.#inputFromHistory(item);
        if (input.feedSeq && input.placement === "feed") {
          entries.push({
            kind: "input",
            key: `input:${input.key}`,
            seq: input.feedSeq,
            input,
          });
        }
      }
    }
    this.timeline.applyHistoryPage(entries, dir);
  }

  #blockFromHistory(item: HistoryBlock, wholesale: boolean): BlockModel {
    let block = this.blocks.get(item.id);
    if (!block) {
      block = new BlockModel(item.id, item.block_kind);
      this.blocks.set(item.id, block);
    }
    block.parentId = item.parent_block_id ?? block.parentId;
    block.agent = item.agent ?? block.agent;
    block.model = item.model ?? block.model;
    block.toolName = item.tool_name ?? block.toolName;
    block.summary = item.summary ?? block.summary;
    block.approvedBy = item.approved_by ?? block.approvedBy;
    block.inputCommandId = item.input_command_id ?? block.inputCommandId;
    block.createdSeq = item.created_seq;
    block.createdAt = Date.parse(item.created_at);
    if (item.started_at) block.startedAt = Date.parse(item.started_at);
    else if (block.startedAt === null) block.startedAt = block.createdAt;
    if (item.finished_at) block.finishedAt = Date.parse(item.finished_at);
    if (item.media) {
      block.media = item.media.map((shot) => ({
        media_id: shot.media_id,
        mime: shot.mime,
        width: shot.width ?? 1,
        height: shot.height ?? 1,
        ready: shot.ready !== false,
      }));
    }
    block.truncated = item.content_truncated;
    block.contentBytes = item.content_bytes;
    if (
      !block ||
      wholesale ||
      block.state === "streaming" ||
      block.state === "stale" ||
      block.body === ""
    ) {
      block.body = item.content;
      block.bytes = utf8Length(item.content);
      block.rev = item.rev;
      block.text =
        item.block_kind === "tool_call" && item.state !== "final"
          ? (item.summary ?? "")
          : item.content;
    }
    // Счёт хаба идёт по всему тексту, а в истории лежит только его начало.
    block.lineCount = item.line_count ?? block.lineCount;
    if (
      item.state === "final" ||
      item.state === "incomplete" ||
      item.state === "streaming" ||
      item.state === "stale"
    ) {
      block.state = item.state;
    }
    block.incompleteReason = item.incomplete_reason ?? block.incompleteReason;
    return block;
  }

  #inputFromHistory(item: HistoryInput): InputView {
    const state = asState(item.state);
    const input =
      this.inputs.get(item.idempotency_key) ??
      freshInput(
        item.idempotency_key,
        textOf(item.content),
        Date.parse(item.created_at),
      );
    input.id = item.id;
    input.text = textOf(item.content);
    input.attachments = item.attachments;
    input.state = state;
    if (item.feed_seq || placeInput(state) !== "strip") input.optimistic = false;
    input.placement = input.optimistic ? "feed" : placeInput(state);
    input.marks = marksFor(state);
    input.localOnly = false;
    input.createdSeq = item.created_seq;
    input.createdAt = Date.parse(item.created_at);
    input.retryOf = item.retry_of ?? null;
    if (item.feed_seq) input.feedSeq = item.feed_seq;
    this.inputs.set(input.key, input);
    this.deps.ops.settle(input.key);
    return input;
  }

  #adoptSaving(): void {
    for (const op of this.deps.ops.ops.values()) {
      if (op.kind !== "input" || op.conversationId !== this.id) continue;
      if (op.state !== "saving" && op.state !== "unknown") continue;
      if (this.inputs.has(op.key)) continue;
      const { text, attachmentIds } = inputOfBody(op.body);
      this.inputs.set(
        op.key,
        freshInput(
          op.key,
          text,
          op.createdAt,
          attachmentIds.map(blankAttachment),
        ),
      );
    }
  }
}

function visible(): boolean {
  return (
    typeof document === "undefined" || document.visibilityState !== "hidden"
  );
}

function seqOf(item: HistoryItem): string {
  if (item.kind === "input") return item.feed_seq || item.created_seq;
  return item.created_seq;
}

function terminalEntry(entry: TimelineEntry): boolean {
  if (entry.kind === "block")
    return entry.block.state === "final" || entry.block.state === "incomplete";
  if (entry.kind === "request") return !isDocked(entry.request);
  return (
    entry.input.state === "accepted" ||
    entry.input.state === "rejected" ||
    entry.input.state === "cancelled" ||
    entry.input.state === "acceptance_unconfirmed"
  );
}
