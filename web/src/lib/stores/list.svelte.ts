import { SvelteMap, SvelteSet } from "svelte/reactivity";
import type { components } from "#lib/api/generated/openapi.js";
import type {
  AppliedEvent,
  RangeResult,
  ScopeApplier,
  ScopeStatus,
} from "#lib/sync/scope.js";
import type { ConnectionManager } from "#lib/sync/connection.js";
import type { HubEvent } from "#lib/sync/wire.js";
import type { LimitWindow, ProviderLimits, WindowForecast } from "./limits";
import { PlanModel, type PlanTask } from "./plan.svelte";

/**
 * Прогноз из события `limits_changed`: поле, если хаб его прислал; иначе прежний прогноз,
 * пока окно то же (тот же сброс), — до следующего снимка он верен приблизительно.
 */
function eventForecast(
  payload: { resets_at: string | null },
  previous: LimitWindow | null | undefined,
): WindowForecast | null {
  if ("forecast" in payload) return (payload as { forecast?: WindowForecast | null }).forecast ?? null;
  return previous && previous.resets_at === payload.resets_at ? (previous.forecast ?? null) : null;
}

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

export interface GroupPin {
  group_id: string;
  pinned_at: string;
}

export interface ListRow {
  id: string;
  title: string;
  title_source: string;
  color: string | null;
  pinned: boolean;
  archived: boolean;
  /** Сессия принята родителем: архив скрывает её до срока хранения. */
  accepted?: boolean;
  /** Сессия в корзине владельца; время окончательной уборки. */
  trashed_at?: string | null;
  groups: string[];
  group_pins: GroupPin[];
  parent_id: string | null;
  spawn_depth: number;
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
  /** Сумма ходов без ожидания владельца; у живой сессии — вместе с текущим ходом. */
  working_ms?: number;
  /** Решение маршрута, выбравшее исполнителя; `null` — запуск без маршрута. */
  routing?: unknown;
}

export interface GroupCount {
  id: string;
  count: number;
}

export interface Counts {
  all: number;
  pinned: number;
  archived: number;
  trashed: number;
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
  /** Задачи планов оркестраторов: снимок заменяет набор целиком. */
  plan_tasks?: PlanTask[];
}

const ATTRIBUTES = [
  "title",
  "title_source",
  "color",
  "pinned",
  "archived",
  "accepted",
  "trashed",
  "trashed_at",
  "groups",
  "group_pins",
  "parent_id",
  "spawn_depth",
  "machine",
  "harness",
  "workdir",
  "created_at",
] as const;

export class ListStore {
  readonly items = new SvelteMap<string, ListRow>();
  /** Сессии, удалённые на глазах у этого клиента: поиск и экраны держат их копии. */
  readonly deleted = new SvelteSet<string>();
  /** Планы оркестраторов: задачи и их ревизии. */
  readonly plan = new PlanModel();
  groups = $state<ListSnapshot["groups"]>([]);
  counts = $state<Counts>({ all: 0, pinned: 0, archived: 0, trashed: 0, groups: [] });
  limits = $state<ProviderLimits[]>([]);
  presets = $state<PresetSummary[]>([]);
  /** Ревизия политики маршрутизации по событию: редактор и подписи маршрута перечитывают её. */
  routingRevision = $state<number | null>(null);
  truncated = $state(false);
  unknownKinds = $state(0);

  #sub: { release(): void; readonly status: ScopeStatus } | null = null;
  #onDeleted: ((id: string) => void)[] = [];

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

  onDeleted(fn: (id: string) => void): () => void {
    this.#onDeleted.push(fn);
    return () => {
      this.#onDeleted = this.#onDeleted.filter((item) => item !== fn);
    };
  }

  #markDeleted(id: string): void {
    this.items.delete(id);
    this.deleted.add(id);
    this.plan.dropOrchestrator(id);
    for (const fn of this.#onDeleted) fn(id);
  }

  close(): void {
    this.#sub?.release();
    this.#sub = null;
  }

  async search(q: string, filter: ListFilter = "all"): Promise<ListRow[]> {
    const needle = q.trim().toLowerCase();
    return [...this.items.values()].filter((item) => {
      if (needle && !item.title.toLowerCase().includes(needle)) return false;
      // Корзина живёт отдельной вкладкой и в обычный поиск не попадает.
      if (item.trashed_at) return false;
      if (filter === "archived") return item.archived && !item.accepted;
      if (filter === "pinned") return item.pinned && !item.archived;
      if (typeof filter === "object")
        return item.groups.includes(filter.group) && !item.archived;
      return !item.archived;
    });
  }

  #applySnapshot(data: ListSnapshot): void {
    // Клиент без связи узнаёт об удалении при сверке: полный снимок больше не содержит строку.
    const fresh = (data.items ?? []).map((item) => item.id);
    const vanished = data.truncated
      ? []
      : [...this.items.keys()].filter((id) => !fresh.includes(id));
    this.items.clear();
    for (const item of data.items ?? []) this.items.set(item.id, item);
    for (const id of vanished) this.#markDeleted(id);
    this.groups = data.groups ?? [];
    this.counts = data.counts ?? this.counts;
    this.limits = data.limits ?? [];
    this.presets = data.presets ?? [];
    this.truncated = Boolean(data.truncated);
    this.plan.replace((data.plan_tasks ?? []).filter((task) => !this.deleted.has(task.orchestrator_id)));
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
    if (kind === "conversation_deleted" && event.kind === "conversation_deleted") {
      this.#markDeleted(event.payload.id);
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
      row.working_ms = event.payload.working_ms;
      row.model = event.payload.settings.model;
      row.effort = event.payload.settings.effort;
      row.permission_mode = event.payload.settings.permission_mode;
      row.crunch = { enabled: event.payload.crunch.enabled };
      this.items.set(row.id, { ...row });
      return;
    }
    if (kind === "plan_task_changed" && event.kind === "plan_task_changed") {
      const task = event.payload.task;
      // Событие могло прийти позже удаления оркестратора: план удалённой сессии не возвращается.
      if (!this.deleted.has(task.orchestrator_id)) this.plan.apply(task);
      return;
    }
    if (kind === "group_changed" && event.kind === "group_changed") {
      this.groups = event.payload.groups;
      this.counts = event.payload.counts;
      return;
    }
    if (kind === "instruction_preset_changed" && event.kind === "instruction_preset_changed") {
      const preset = event.payload.preset as PresetSummary;
      // Тексты толчка живут только во вкладке «Текст толчка» и читаются запросом.
      if (preset.purpose === "nudge") return;
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
      const previous =
        payload.window === "five_hour" ? provider.five_hour : payload.window === "monthly" ? provider.monthly : provider.weekly;
      // Событие без значения сообщает, что пятичасового окна у провайдера нет.
      const window: LimitWindow | null =
        payload.used_permille === null
          ? null
          : {
              used_permille: payload.used_permille,
              resets_at: payload.resets_at,
              observed_at: payload.observed_at,
              forecast: eventForecast(payload, previous),
            };
      if (payload.window === "five_hour") {
        provider.five_hour = window;
        provider.five_hour_absent = window === null;
      } else if (payload.window === "monthly") {
        provider.monthly = window;
      } else if (window) provider.weekly = window;
      provider.weekly_reserve_active = payload.weekly_reserve_active;
      provider.weekly_reserve_resets_at = payload.weekly_reserve_resets_at;
      if (payload.data_confidence) provider.data_confidence = payload.data_confidence;
      this.limits = next;
      return;
    }
    if (kind === "limits_providers_changed" && event.kind === "limits_providers_changed") {
      // Харнесс провайдера ушёл из каталога машин: его лимиты больше не показываются.
      const offered = event.payload.providers;
      this.limits = this.limits.filter((item) => offered.includes(item.provider));
      return;
    }
    if (kind === "routing_policy_changed" && event.kind === "routing_policy_changed") {
      this.routingRevision = event.payload.revision ?? (this.routingRevision ?? 0) + 1;
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
  accepted?: boolean;
  trashed?: boolean;
  trashed_at?: string | null;
  groups: string[];
  group_pins?: GroupPin[];
  parent_id?: string | null;
  spawn_depth?: number;
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
  return {
    parent_id: null,
    spawn_depth: 0,
    group_pins: [],
    ...payload,
    activity: payload.activity,
  };
}

function rowFromUpdated(payload: {
  id: string;
  title: string;
  title_source: string;
  color: string | null;
  pinned: boolean;
  archived: boolean;
  accepted?: boolean;
  trashed?: boolean;
  trashed_at?: string | null;
  groups: string[];
  group_pins?: GroupPin[];
  machine: ListRow["machine"];
  harness: ListRow["harness"];
  workdir: string;
  created_at: string;
  parent_id?: string | null;
  spawn_depth?: number;
}): ListRow {
  return {
    parent_id: null,
    spawn_depth: 0,
    group_pins: [],
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
