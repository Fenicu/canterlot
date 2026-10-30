import { SvelteMap } from "svelte/reactivity";
import type { components } from "#lib/api/generated/openapi.js";
import type {
  AppliedEvent,
  RangeResult,
  ScopeApplier,
  ScopeStatus,
} from "#lib/sync/scope.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import type { HubEvent } from "#lib/sync/wire.js";
import type { ProviderLimits } from "./limits";

export interface ListActivity {
  state:
    | "idle"
    | "working"
    | "waiting_for_user"
    | "stalled"
    | "quota_paused"
    | "failed"
    | "stopped";
  evidence?: unknown;
  since?: string;
}

export interface ListRow {
  id: string;
  title: string;
  title_source: string;
  color: string | null;
  pinned: boolean;
  archived: boolean;
  groups: string[];
  machine: { id: string; name: string };
  harness: { id: string; title: string; provider: string };
  workdir: string;
  created_at: string;
  model: string;
  effort: string;
  permission_mode: string;
  activity: ListActivity;
  unread_count: number;
  queue: { queued: number; pause_reasons: unknown[]; hold_reasons: unknown[] };
  crunch: { enabled: boolean };
  last_activity_at: string | null;
}

export interface GroupCount {
  id: string;
  count: number;
}

export interface Counts {
  all: number;
  pinned: number;
  archived: number;
  groups: GroupCount[];
}

export type ListFilter = "all" | "pinned" | "archived" | { group: string };

export type PresetSummary = components["schemas"]["PresetSummary"];

export interface ListSnapshot {
  counts: Counts;
  groups: { id: string; name: string; position: number; created_at: string }[];
  items: ListRow[];
  limits: ProviderLimits[];
  truncated: boolean;
  presets: PresetSummary[];
}

const ATTRIBUTES = [
  "title",
  "title_source",
  "color",
  "pinned",
  "archived",
  "groups",
  "machine",
  "harness",
  "workdir",
  "created_at",
] as const;

export class ListStore {
  readonly items = new SvelteMap<string, ListRow>();
  groups = $state<ListSnapshot["groups"]>([]);
  counts = $state<Counts>({ all: 0, pinned: 0, archived: 0, groups: [] });
  limits = $state<ProviderLimits[]>([]);
  presets = $state<PresetSummary[]>([]);
  truncated = $state(false);
  unknownKinds = $state(0);

  #sub: { release(): void; readonly status: ScopeStatus } | null = null;

  constructor(
    private deps: {
      api: {
        snapshot(): Promise<{ cursor: string; data: ListSnapshot }>;
        events(after: string, limit: number): Promise<RangeResult>;
      };
      connection: ConnectionManager;
    },
  ) {}

  get status(): ScopeStatus {
    return this.#sub?.status ?? "idle";
  }

  get applier(): ScopeApplier {
    return {
      loadSnapshot: async () => {
        const snap = await this.deps.api.snapshot();
        return { cursor: snap.cursor, data: snap.data };
      },
      applySnapshot: (data) => this.#applySnapshot(data as ListSnapshot),
      applyEvents: (batch) => {
        for (const item of batch) this.#apply(item.event);
      },
      fetchRange: (after, limit) => this.deps.api.events(after, limit),
    };
  }

  open(): void {
    if (!this.#sub)
      this.#sub = this.deps.connection.subscribe("conversations", this.applier);
  }

  close(): void {
    this.#sub?.release();
    this.#sub = null;
  }

  async search(q: string, filter: ListFilter = "all"): Promise<ListRow[]> {
    const needle = q.trim().toLowerCase();
    return [...this.items.values()].filter((item) => {
      if (needle && !item.title.toLowerCase().includes(needle)) return false;
      if (filter === "archived") return item.archived;
      if (filter === "pinned") return item.pinned && !item.archived;
      if (typeof filter === "object")
        return item.groups.includes(filter.group) && !item.archived;
      return !item.archived;
    });
  }

  #applySnapshot(data: ListSnapshot): void {
    this.items.clear();
    for (const item of data.items ?? []) this.items.set(item.id, item);
    this.groups = data.groups ?? [];
    this.counts = data.counts ?? this.counts;
    this.limits = data.limits ?? [];
    this.presets = data.presets ?? [];
    this.truncated = Boolean(data.truncated);
  }

  #apply(event: HubEvent): void {
    const kind = event.kind as string;
    if (kind === "hidden") return;
    if (
      kind === "conversation_created" &&
      event.kind === "conversation_created"
    ) {
      this.items.set(event.payload.id, rowFromCreated(event.payload));
      this.counts = event.payload.counts;
      return;
    }
    if (
      kind === "conversation_updated" &&
      event.kind === "conversation_updated"
    ) {
      const row = this.items.get(event.payload.id);
      if (row) {
        copyAttributes(row, event.payload);
        this.items.set(row.id, { ...row });
      } else this.items.set(event.payload.id, rowFromUpdated(event.payload));
      this.counts = event.payload.counts;
      return;
    }
    if (kind === "derived_changed" && event.kind === "derived_changed") {
      const row = this.items.get(event.payload.conversation_id);
      if (!row) return;
      row.activity = event.payload.activity as ListActivity;
      row.unread_count = event.payload.unread_count;
      row.queue = event.payload.queue;
      row.last_activity_at = event.payload.last_activity_at;
      row.model = event.payload.settings.model;
      row.effort = event.payload.settings.effort;
      row.permission_mode = event.payload.settings.permission_mode;
      row.crunch = { enabled: event.payload.crunch.enabled };
      this.items.set(row.id, { ...row });
      return;
    }
    if (kind === "group_changed" && event.kind === "group_changed") {
      this.groups = event.payload.groups;
      this.counts = event.payload.counts;
      return;
    }
    if (kind === "instruction_preset_changed" && event.kind === "instruction_preset_changed") {
      const preset = event.payload.preset as PresetSummary;
      const next = this.presets.filter((item) => item.id !== preset.id);
      if (!event.payload.deleted) next.push(preset);
      const rank = (item: PresetSummary): number => item.kind === "clean" ? 0 : item.kind === "owner" ? 1 : 2;
      next.sort((a, b) => rank(a) - rank(b) || a.title.localeCompare(b.title, "ru"));
      this.presets = next;
      return;
    }
    if (kind === "limits_changed" && event.kind === "limits_changed") {
      const payload = event.payload;
      const next = this.limits.map((item) => ({ ...item }));
      let provider = next.find((item) => item.provider === payload.provider);
      if (!provider) {
        provider = { provider: payload.provider, weekly_reserve_active: false };
        next.push(provider);
      }
      const window = {
        used_permille: payload.used_permille,
        resets_at: payload.resets_at,
        observed_at: payload.observed_at,
      };
      if (payload.window === "five_hour") provider.five_hour = window;
      else provider.weekly = window;
      provider.weekly_reserve_active = payload.weekly_reserve_active;
      provider.weekly_reserve_resets_at = payload.weekly_reserve_resets_at;
      this.limits = next;
      return;
    }
    this.unknownKinds += 1;
  }
}

function copyAttributes(row: ListRow, payload: Record<string, unknown>): void {
  for (const key of ATTRIBUTES) {
    if (key in payload)
      (row as unknown as Record<string, unknown>)[key] = payload[key];
  }
}

function rowFromCreated(payload: {
  id: string;
  title: string;
  title_source: string;
  color: string | null;
  pinned: boolean;
  archived: boolean;
  groups: string[];
  machine: { id: string; name: string };
  harness: { id: string; title: string; provider: string };
  workdir: string;
  created_at: string;
  model: string;
  effort: string;
  permission_mode: string;
  activity: ListActivity;
  unread_count: number;
  queue: ListRow["queue"];
  crunch: { enabled: boolean };
  last_activity_at: string | null;
}): ListRow {
  return { ...payload, activity: payload.activity };
}

function rowFromUpdated(payload: {
  id: string;
  title: string;
  title_source: string;
  color: string | null;
  pinned: boolean;
  archived: boolean;
  groups: string[];
  machine: ListRow["machine"];
  harness: ListRow["harness"];
  workdir: string;
  created_at: string;
}): ListRow {
  return {
    ...payload,
    model: "",
    effort: "",
    permission_mode: "",
    activity: { state: "idle" },
    unread_count: 0,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
    crunch: { enabled: false },
    last_activity_at: null,
  };
}

export type { AppliedEvent };
