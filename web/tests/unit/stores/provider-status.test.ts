import { describe, expect, it } from "vitest";
import { statusBadge, type ProviderStatus } from "#lib/stores/limits.js";
import { ListStore } from "#lib/stores/list.svelte.js";
import { limitStrip } from "#lib/screens/sessions/limitsText.js";
import { DEFAULT_LIMIT_THRESHOLDS } from "#lib/stores/limits.js";
import type { HubEvent } from "#lib/sync/wire.js";
import type { ConnectionManager } from "#lib/sync/connection.js";

function status(level: ProviderStatus["level"], extra: Partial<ProviderStatus> = {}): ProviderStatus {
  return {
    level,
    description: "Partial System Outage",
    components: ["Claude Code"],
    incidents: [{ name: "Elevated errors", url: "https://stspg.io/x" }],
    url: "https://status.claude.com",
    fetched_at: "2026-11-12T16:00:00.000Z",
    changed_at: "2026-11-12T16:02:00.000Z",
    stale: false,
    ...extra,
  };
}

function ev(kind: string, payload: unknown, seq: string): HubEvent {
  return {
    v: 1,
    id: "00000000-0000-4000-8000-0000000000ee",
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: "conversations",
    scope_seq: seq,
    scope_seq_from: seq,
    committed_at: "2026-11-12T16:02:00.000Z",
    committed_at_from: "2026-11-12T16:02:00.000Z",
    conversation_id: null,
    execution_id: null,
    kind,
    payload,
  } as HubEvent;
}

function listOf(): ListStore {
  return new ListStore({
    connection: {} as ConnectionManager,
    api: {
      snapshot: async () => ({
        cursor: "0",
        data: {
          counts: { all: 0, pinned: 0, archived: 0, trashed: 0, groups: [] },
          groups: [],
          items: [],
          limits: [],
          truncated: false,
          presets: [],
        },
      }),
      events: async () => ({ events: [], head: "0" }),
    },
  });
}

describe("provider status", () => {
  it("badge_words_and_tones_follow_the_level", () => {
    expect(statusBadge("claude", status("major_outage"))).toMatchObject({ text: "сбой", tone: "danger", company: "Anthropic" });
    expect(statusBadge("codex", status("partial_outage"))).toMatchObject({ text: "частичный сбой", tone: "danger", company: "OpenAI" });
    expect(statusBadge("claude", status("degraded"))).toMatchObject({ text: "замедление", tone: "warning" });
    expect(statusBadge("claude", status("maintenance"))).toMatchObject({ text: "обслуживание", tone: "warning" });
    expect(statusBadge("claude", status("ok"))).toBeNull();
    expect(statusBadge("claude", status("unknown", { stale: true }))).toBeNull();
    expect(statusBadge("claude", null)).toBeNull();
    expect(statusBadge("claude", status("major_outage"))?.title).toBe(
      "Anthropic: сбой (Claude Code; Partial System Outage). Открыть страницу статуса",
    );
  });

  it("strip_rows_carry_the_badge_only_when_notable", () => {
    const window = { used_permille: 100, resets_at: null, observed_at: "2026-11-12T16:00:00.000Z" };
    const strip = limitStrip(
      [
        { provider: "claude", weekly: window, weekly_reserve_active: false, status: status("partial_outage") },
        { provider: "codex", weekly: window, weekly_reserve_active: false, status: status("ok") },
        { provider: "opencode", weekly: window, weekly_reserve_active: false },
      ],
      Date.parse("2026-11-12T16:05:00.000Z"),
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(strip.rows.map((row) => row.status?.text ?? null)).toEqual(["частичный сбой", null, null]);
  });

  it("provider_status_changed_updates_the_strip_and_waits_for_a_provider_not_yet_listed", () => {
    const list = listOf();
    const window = { used_permille: 10, resets_at: null, observed_at: "2026-11-12T16:00:00.000Z" };
    list.limits = [{ provider: "claude", weekly: window, weekly_reserve_active: false }];
    list.applier.applyEvents([
      { event: ev("provider_status_changed", { provider: "claude", status: status("major_outage") }, "5"), overlap: false },
      { event: ev("provider_status_changed", { provider: "codex", status: status("degraded") }, "6"), overlap: false },
    ]);
    expect(list.limits).toHaveLength(1);
    expect(list.limits[0]!.status?.level).toBe("major_outage");
    list.applier.applyEvents([
      {
        event: ev(
          "limits_changed",
          {
            provider: "codex",
            window: "weekly",
            used_permille: 300,
            resets_at: null,
            observed_at: "2026-11-12T16:03:00.000Z",
            weekly_reserve_active: false,
            weekly_reserve_resets_at: null,
          },
          "7",
        ),
        overlap: false,
      },
      { event: ev("provider_status_changed", { provider: "claude", status: status("ok") }, "8"), overlap: false },
    ]);
    expect(list.limits.find((item) => item.provider === "codex")?.status?.level).toBe("degraded");
    expect(list.limits.find((item) => item.provider === "claude")?.status?.level).toBe("ok");
    expect(list.unknownKinds).toBe(0);
  });
});
