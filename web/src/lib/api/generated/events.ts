export type WireEvent =
  | InputQueued
  | InputStateChanged
  | QueueStateChanged
  | QueuePaused
  | ExecutionStateChanged
  | RunFinished
  | InterruptStateChanged
  | BlockStarted
  | BlockDelta
  | BlockFinal
  | BlockIncomplete
  | AdapterDiagnostic
  | StateDivergence
  | BlockRevisionGap
  | BlockChecksumMismatch
  | BlockConflictingFinal
  | BlockMediaAttached
  | BlockFileAttached
  | ConversationCreated
  | ConversationUpdated
  | ConversationDeleted
  | ConversationSettingsChanged
  | CrunchChanged
  | CrunchHeld
  | CrunchWoken
  | DerivedChanged
  | GroupChanged
  | ScheduledChanged
  | AlarmChanged
  | NudgeChanged
  | LimitsChanged
  | LimitReached
  | AttachmentStateChanged
  | RequestOpened
  | RequestClosed
  | RequestDecided
  | RequestTransmitting
  | TodoUpdated
  | BackgroundWorkChanged
  | MachineAvailabilityChanged
  | MachineReportUpdated
  | MachineMetrics
  | MachineJournalStopsChanged
  | Hidden
  | PhaseChanged
  | Oversized
  | QueueResumed
  | InstructionPresetChanged
  | CompactionStarted
  | CompactionFinished
  | ContextChanged;
export type Uuid = string;
export type Scope = string;
export type Seq = string;
export type Time = string;
export type ContentBlock =
  | {
      type: "text";
      text: string;
    }
  | {
      type: "attachment";
      attachment_id: Uuid;
      name: string;
      mime: string;
      size: number;
      sha256: string;
      image: boolean;
    };
export type PresetKind = "clean" | "owner" | "custom";

export interface InputQueued {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "input_queued";
  payload: {
    input_id: Uuid;
    input_seq: Seq;
    idempotency_key: string;
    state: "queued";
    content: ContentBlock[];
    origin?: InputOrigin | null;
  };
}
export interface InputOrigin {
  kind: "message" | "spawn" | "event" | "alarm" | "nudge";
  from?: Uuid;
  title?: string;
  harness?: string;
  model?: string;
  chain_depth: number;
  sha256?: string;
  sender_deleted?: boolean;
  event?: HubEvent;
}
export interface HubEvent {
  kind: string;
  state?: string;
  session_id?: Uuid;
  turns?: number;
  duration_ms?: number;
  working_ms?: number;
  tokens_in?: number;
  tokens_out?: number;
  commits?: HubCommit[];
  changed_files?: number;
  answer?: string;
  answer_bytes?: number;
  answer_block_id?: Uuid;
  question?: string;
  phase?: string;
  last_event_at?: Time;
  process_alive?: boolean;
  cpu_busy?: boolean;
  reason?: string;
  provider?: string;
  window?: string;
  until?: Time;
  outcome?: string;
  worktree?: string;
  branch?: string;
  branch_kept?: boolean;
  decision?: string;
  request_id?: Uuid;
  child_id?: Uuid;
  closed?: boolean;
  detail?: string;
  nudges?: number;
  nudge_exhausted?: boolean;
  asks_in_text?: boolean;
}
export interface HubCommit {
  sha: string;
  title: string;
}
export interface InputStateChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "input_state_changed";
  payload: {
    input_id: Uuid;
    state:
      | "queued"
      | "dispatched"
      | "on_machine"
      | "transmitting"
      | "accepted"
      | "acceptance_unconfirmed"
      | "cancelled"
      | "rejected";
    attempt?: number;
    cause?: string;
    revision?: number;
    content?: ContentBlock[];
    command_id?: Uuid;
    reason?: string;
    retry_of?: Uuid | null;
  };
}
export interface QueueStateChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "queue_state_changed";
  payload: {
    pause_reasons: {}[];
    hold_reasons: {}[];
    run_slot:
      | "free"
      | {
          dispatching: {
            input_id: Uuid;
            attempt: number;
          };
        }
      | {
          running: {
            input_id: Uuid;
          };
        };
  };
}
export interface QueuePaused {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "queue_paused";
  payload: {
    pause_reasons: {}[];
  };
}
export interface ExecutionStateChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "execution_state_changed";
  payload: {
    state: "created" | "starting" | "running" | "stopping" | "stopped" | "failed" | "owner_lost";
    generation: number;
    outcome: string | null;
  };
}
export interface RunFinished {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "run_finished";
  payload: {
    input_id: Uuid;
    input_command_id: Uuid;
  };
}
export interface InterruptStateChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "interrupt_state_changed";
  payload: {
    command_id: Uuid;
    status: "sent_to_agent" | "acknowledged" | "completed" | "not_applicable" | "superseded" | "rejected_stale";
  };
}
export interface BlockStarted {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_started";
  payload: {
    block_id: Uuid;
    rev: number;
    kind: "text" | "reasoning" | "tool_call" | "tool_result" | "warning" | "error";
    input_command_id: Uuid | null;
    parent_block_id: Uuid | null;
    agent: string;
    model: string;
    tool_name: string | null;
    summary: string | null;
    approved_by: string | null;
  };
}
export interface BlockDelta {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_delta";
  payload: {
    block_id: Uuid;
    base_rev: number;
    rev: number;
    append: string;
  };
}
export interface BlockFinal {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_final";
  payload: {
    block_id: Uuid;
    rev: number;
    text: string;
    checksum: string;
    text_omitted: boolean;
    summary: string | null;
  };
}
export interface BlockIncomplete {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_incomplete";
  payload: {
    block_id: Uuid;
    rev: number;
    reason: string;
  };
}
export interface AdapterDiagnostic {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "adapter_diagnostic";
  payload: {
    kind: string;
    event_type: string;
    /**
     * @maxItems 32
     */
    field_names?: string[];
  };
}
export interface StateDivergence {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "state_divergence";
  payload: {
    machine_id?: Uuid;
    execution_id?: Uuid;
    reason?: string;
    expected?: string;
    observed?: string;
  };
}
export interface BlockRevisionGap {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_revision_gap";
  payload: {
    block_id: Uuid;
    current_rev: number;
    base_rev: number;
    rev: number;
  };
}
export interface BlockChecksumMismatch {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_checksum_mismatch";
  payload: {
    block_id: Uuid;
    rev: number;
  };
}
export interface BlockConflictingFinal {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_conflicting_final";
  payload: {
    block_id: Uuid;
    rev: number;
  };
}
export interface BlockMediaAttached {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_media_attached";
  payload: {
    block_id: Uuid;
    media_id: Uuid;
    mime: "image/png" | "image/jpeg" | "image/webp" | "image/gif";
    size: number;
    width: number | null;
    height: number | null;
  };
}
export interface BlockFileAttached {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "block_file_attached";
  payload: {
    block_id: Uuid;
    attachment_id: Uuid;
    name: string;
    mime: string;
    size: number;
    title: string;
    note: string;
    width: number | null;
    height: number | null;
  };
}
export interface ConversationCreated {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "conversation_created";
  payload: {
    id: Uuid;
    title: string;
    title_source: "auto" | "user";
    color: string | null;
    pinned: boolean;
    archived: boolean;
    accepted?: boolean;
    groups: Uuid[];
    group_pins?: GroupPin[];
    machine: {
      id: Uuid;
      name: string;
    };
    harness: {
      id: string;
      title: string;
      provider: string;
    };
    workdir: string;
    created_at: Time;
    model: string;
    effort: string;
    permission_mode: string;
    activity: Activity;
    unread_count: number;
    queue: Queue;
    crunch: {
      enabled: boolean;
    };
    last_activity_at: Time | null;
    working_ms?: number;
    counts: Counts;
    parent_id?: Uuid | null;
    spawn_depth?: number;
    worktree?: {
      branch: string;
      state: string;
      path: string;
    } | null;
  };
}
export interface GroupPin {
  group_id: Uuid;
  pinned_at: Time;
}
export interface Activity {
  state: "idle" | "working" | "waiting_for_user" | "stalled" | "quota_paused" | "failed" | "stopped";
  evidence: {};
  since: Time;
}
export interface Queue {
  queued: number;
  pause_reasons: {}[];
  hold_reasons: {}[];
}
export interface Counts {
  all: number;
  pinned: number;
  archived: number;
  groups: {
    id: Uuid;
    count: number;
  }[];
}
export interface ConversationUpdated {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "conversation_updated";
  payload: {
    id: Uuid;
    title: string;
    title_source: "auto" | "user";
    color: string | null;
    pinned: boolean;
    archived: boolean;
    accepted?: boolean;
    groups: Uuid[];
    group_pins?: GroupPin[];
    machine: {
      id: Uuid;
      name: string;
    };
    harness: {
      id: string;
      title: string;
      provider: string;
    };
    workdir: string;
    created_at: Time;
    counts: Counts;
    parent_id?: Uuid | null;
    spawn_depth?: number;
    worktree?: {
      branch: string;
      state: string;
      path: string;
    } | null;
  };
}
export interface ConversationDeleted {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "conversation_deleted";
  payload: {
    id: Uuid;
    counts: Counts;
  };
}
export interface ConversationSettingsChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "conversation_settings_changed";
  payload: {
    effective: {};
    pending: {};
    rev: number;
    rejected?: {
      keys: string[];
      reason: string;
    };
  };
}
export interface CrunchChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "crunch_changed";
  payload: {
    enabled: boolean;
    resume_text?: string | null;
    reserve_override_until: Time | null;
    resume_pending: boolean;
  };
}
export interface CrunchHeld {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "crunch_held";
  payload: {
    provider: string;
    window: "five_hour" | "weekly";
    reason: "quota_exhausted" | "weekly_reserve";
    wake_at: Time | null;
    resume_pending: boolean;
  };
}
export interface CrunchWoken {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "crunch_woken";
  payload: {
    provider: string;
    window: "five_hour" | "weekly";
    hold_key?: string;
    input_id: Uuid | null;
    skipped:
      | (
          | "closed"
          | "crunch_disabled"
          | "no_live_execution"
          | "resume_already_queued"
          | "queue_not_empty"
          | "not_pending"
          | "paused"
          | "weekly_reserve"
        )
      | null;
    resume_pending: boolean;
  };
}
export interface DerivedChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "derived_changed";
  payload: {
    conversation_id: Uuid;
    activity: Activity;
    unread_count: number;
    queue: Queue;
    last_activity_at: Time | null;
    settings: Settings;
    crunch: {
      enabled: boolean;
    };
    working_ms?: number;
  };
}
export interface Settings {
  model: string;
  effort: string;
  permission_mode: string;
}
export interface GroupChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "group_changed";
  payload: {
    group: Group | null;
    groups: Group[];
    counts: Counts;
  };
}
export interface Group {
  id: Uuid;
  name: string;
  position: number;
  created_at: Time;
}
export interface ScheduledChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "scheduled_changed";
  payload: {
    id: Uuid;
    conversation_id: Uuid;
    run_at: Time;
    content: ContentBlock[];
    attachment_ids: Uuid[];
    state: "pending" | "fired" | "cancelled" | "failed";
    input_id: Uuid | null;
    attempts: number;
  };
}
export interface AlarmChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "alarm_changed";
  payload: {
    alarm_id: Uuid;
    conversation_id: Uuid;
    key: string | null;
    kind: string;
    condition: string;
    provider?: string | null;
    window?: string | null;
    below_percent?: number | null;
    free_disk_gb_above?: number | null;
    load_per_core_below?: number | null;
    mem_available_gb_above?: number | null;
    session_ids: Uuid[];
    states: string[];
    fires_at: Time | null;
    deadline_at: Time | null;
    only_on_timeout: boolean;
    batch: boolean;
    state: "armed" | "fired" | "cancelled" | "expired" | "failed";
    fired_reason: string | null;
    failure_reason: string | null;
    fired_at: Time | null;
    note: string | null;
    note_bytes: number;
    created_at: Time;
  };
}
export interface NudgeChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "nudge_changed";
  payload: {
    session_id?: Uuid;
    enabled?: boolean;
    state?: string;
    idle_minutes?: number;
    background_minutes?: number;
    max_consecutive?: number;
    max_total?: number;
    done_markers?: string[];
    context_warn_percent?: number | null;
    set_by?: string;
    set_by_session?: Uuid | null;
    notify_session_id?: Uuid | null;
    consecutive?: number;
    total?: number;
    held?: boolean;
    detail?: string;
  };
}
export interface LimitsChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "limits_changed";
  payload: {
    provider: string;
    window: "five_hour" | "weekly" | "monthly";
    used_permille: number | null;
    resets_at: Time | null;
    observed_at: Time;
    weekly_reserve_active: boolean;
    weekly_reserve_resets_at: Time | null;
    data_confidence?: "exact" | "estimated";
  };
}
export interface LimitReached {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "limit_reached";
  payload: {
    provider: string;
    window: "five_hour" | "weekly";
    resets_at: Time | null;
  };
}
export interface AttachmentStateChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "attachment_state_changed";
  payload: {
    attachment_id: Uuid;
    state?: "created" | "uploading" | "ready" | "failed" | "cancelled" | "released";
    reason?: string | null;
    input_id?: Uuid;
    machine_state?: "pending" | "ready" | "failed";
    machine_reason?: string | null;
  };
}
export interface RequestOpened {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "request_opened";
  payload: {
    request_id: Uuid;
    kind: string;
    payload: {};
  };
}
export interface RequestClosed {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "request_closed";
  payload: {
    request_id: Uuid;
    reason: string;
  };
}
export interface RequestDecided {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "request_decided";
  payload: {
    request_id: Uuid;
    winner_decision_id: Uuid;
    decision: {};
  };
}
export interface RequestTransmitting {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "request_transmitting";
  payload: {
    request_id: Uuid;
  };
}
export interface TodoUpdated {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "todo_updated";
  payload: {
    lane_block_id: Uuid | null;
    revision: number;
    items: {
      text: string;
      status: "pending" | "in_progress" | "completed" | "cancelled";
    }[];
    omitted: number;
  };
}
export interface BackgroundWorkChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "background_work_changed";
  payload: {
    tasks: {
      task_id: string;
      kind: "agent" | "bash";
      block_id: Uuid | null;
      parent_task_id: string | null;
      description: string;
      state: "running" | "completed" | "failed" | "stopped";
      started_at: Time;
      ended_at: Time | null;
    }[];
  };
}
export interface MachineAvailabilityChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "machine_availability_changed";
  payload: {
    machine_id: Uuid;
    availability: "available" | "degraded" | "unavailable";
  };
}
export interface MachineReportUpdated {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "machine_report_updated";
  payload: {
    machine_id: Uuid;
    worker_version: string;
    capabilities: string[];
    limits_ok: boolean;
    home_dir?: string | null;
  };
}
export interface MachineMetrics {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "machine_metrics";
  payload: {
    machine_id: Uuid;
    machine_time_ms: number;
    cpu_cores: number;
    cpu_permille: number;
    mem_total_bytes: number;
    mem_available_bytes: number;
    load1_centi: number;
    load5_centi: number;
    load15_centi: number;
    disks: {}[];
    executions: {}[];
  };
}
export interface MachineJournalStopsChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "machine_journal_stops_changed";
  payload: {
    machine_id: Uuid;
    stopped: ("cj" | "sj")[];
  };
}
export interface Hidden {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: null;
  execution_id: null;
  correlation_id: Uuid;
  kind: "hidden";
  payload: {};
}
export interface PhaseChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "phase_changed";
  payload: {
    phase:
      | "awaiting_input"
      | "generating"
      | "running_tool"
      | "awaiting_permission"
      | "awaiting_answer"
      | "context_maintenance"
      | "unknown";
    tool_name: string | null;
    summary: string | null;
  };
}
/**
 * WSS-заполнитель одиночного события, превышающего бюджет данных соединения; полный факт доступен по payload.href.
 */
export interface Oversized {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "oversized";
  payload: {
    kind: string;
    href: string;
  };
}
export interface QueueResumed {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "queue_resumed";
  payload: {
    pause_reasons: {}[];
  };
}
export interface InstructionPresetChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "instruction_preset_changed";
  payload: {
    preset: PresetSummary;
    deleted: boolean;
  };
}
export interface PresetSummary {
  id: string;
  kind: PresetKind;
  title: string;
  description: string;
  revision?: number | null;
  text_bytes?: number | null;
  created: PresetOrigin;
  last_edit?: PresetOrigin | null;
  used_by: number;
}
export interface PresetOrigin {
  by: string;
  conversation_id: Uuid | null;
  conversation_title: string | null;
  at?: Time | null;
}
export interface CompactionStarted {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "compaction_started";
  payload: {
    id: Uuid;
    trigger: string;
  };
}
export interface CompactionFinished {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "compaction_finished";
  payload: {
    id: Uuid;
    trigger: string;
    ok: boolean;
  };
}
export interface ContextChanged {
  v: 1;
  id: Uuid;
  scope: Scope;
  scope_seq: Seq;
  scope_seq_from: Seq;
  committed_at: Time;
  committed_at_from: Time;
  conversation_id: Uuid | null;
  execution_id: Uuid | null;
  correlation_id: Uuid;
  kind: "context_changed";
  payload: {
    tokens: number | null;
    window_tokens: number | null;
  };
}
