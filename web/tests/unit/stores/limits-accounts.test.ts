import { describe, expect, it } from "vitest";
import { ListStore } from "#lib/stores/list.svelte.js";
import { DEFAULT_LIMIT_THRESHOLDS, limitKey, type ProviderLimits } from "#lib/stores/limits.js";
import { limitStrip } from "#lib/screens/sessions/limitsText.js";
import type { HubEvent } from "#lib/sync/wire.js";
import type { ConnectionManager } from "#lib/sync/connection.js";

const AT = "2026-11-20T10:00:00.000Z";
const NOW = Date.parse("2026-11-20T10:01:00.000Z");

function ev(kind: string, payload: unknown, seq: string): HubEvent {
  return {
    v: 1,
    id: "00000000-0000-4000-8000-0000000000ee",
    correlation_id: "00000000-0000-4000-8000-0000000000ef",
    scope: "conversations",
    scope_seq: seq,
    scope_seq_from: seq,
    committed_at: AT,
    committed_at_from: AT,
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
      snapshot: async () => ({ cursor: "0", data: {} as never }),
      events: async () => ({ events: [], head: "0" }),
    },
  });
}

function weekly(used: number) {
  return { used_permille: used, resets_at: "2026-11-25T10:00:00.000Z", observed_at: AT };
}

function row(provider: string, account: string | undefined, used: number): ProviderLimits {
  return { provider, ...(account ? { account } : {}), weekly: weekly(used), weekly_reserve_active: false };
}

function limitsChanged(provider: string, account: string | undefined, used: number, seq: string, extra: object = {}) {
  return {
    event: ev(
      "limits_changed",
      {
        provider,
        ...(account ? { account } : {}),
        window: "weekly",
        used_permille: used,
        resets_at: "2026-11-25T10:00:00.000Z",
        observed_at: "2026-11-20T10:05:00.000Z",
        weekly_reserve_active: false,
        weekly_reserve_resets_at: null,
        ...extra,
      },
      seq,
    ),
    overlap: false,
  };
}

function snapshot(limits: ProviderLimits[]) {
  const list = listOf();
  list.applier.applySnapshot(
    { items: [], groups: [], counts: {}, limits, presets: [], truncated: false },
    "1",
  );
  return list;
}

describe("limits store with two accounts of one provider", () => {
  it("bootstrap_keeps_both_rows_and_a_legacy_row_without_account_is_the_owner", () => {
    const list = snapshot([row("opencode", undefined, 100), row("opencode", "b-opencode", 400)]);
    expect(list.limits.map((item) => limitKey(item.provider, item.account))).toEqual([
      "opencode",
      "opencode@b-opencode",
    ]);
  });

  it("a_live_update_of_one_account_leaves_the_other_unchanged", () => {
    const list = snapshot([row("opencode", "owner", 100), row("opencode", "b-opencode", 400)]);
    list.applier.applyEvents([limitsChanged("opencode", "b-opencode", 650, "2")]);
    const owner = list.limits.find((item) => (item.account ?? "owner") === "owner")!;
    const other = list.limits.find((item) => item.account === "b-opencode")!;
    expect(owner.weekly?.used_permille).toBe(100);
    expect(other.weekly?.used_permille).toBe(650);
    expect(list.limits).toHaveLength(2);
  });

  it("an_event_without_account_updates_the_owner_row_not_the_other_account", () => {
    const list = snapshot([row("opencode", "owner", 100), row("opencode", "b-opencode", 400)]);
    list.applier.applyEvents([limitsChanged("opencode", undefined, 150, "2")]);
    expect(list.limits.find((item) => item.account === "owner")?.weekly?.used_permille).toBe(150);
    expect(list.limits.find((item) => item.account === "b-opencode")?.weekly?.used_permille).toBe(400);
  });

  it("an_event_of_a_new_account_adds_its_row_and_keeps_the_provider_status", () => {
    const list = snapshot([row("opencode", "owner", 100)]);
    list.applier.applyEvents([limitsChanged("opencode", "b-opencode", 300, "2")]);
    expect(list.limits).toHaveLength(2);
    expect(list.limits[1]).toMatchObject({ provider: "opencode", account: "b-opencode" });
    expect(list.limits[1]!.weekly?.used_permille).toBe(300);
  });

  it("the_provider_status_reaches_every_account_of_the_provider", () => {
    const list = snapshot([row("opencode", "owner", 100), row("opencode", "b-opencode", 400)]);
    const status = {
      level: "degraded", description: "", components: [], incidents: [], url: "https://status.example",
      fetched_at: AT, changed_at: AT, stale: false,
    };
    list.applier.applyEvents([{ event: ev("provider_status_changed", { provider: "opencode", status }, "2"), overlap: false }]);
    expect(list.limits.map((item) => item.status?.level)).toEqual(["degraded", "degraded"]);
  });

  it("removal_of_one_harness_drops_only_its_account", () => {
    const list = snapshot([row("opencode", "owner", 100), row("opencode", "b-opencode", 400), row("claude", undefined, 10)]);
    list.applier.applyEvents([
      {
        event: ev(
          "limits_providers_changed",
          {
            providers: ["opencode", "claude"],
            accounts: [
              { provider: "opencode", account: "owner" },
              { provider: "claude", account: "owner" },
            ],
          },
          "2",
        ),
        overlap: false,
      },
    ]);
    expect(list.limits.map((item) => limitKey(item.provider, item.account))).toEqual(["opencode", "claude"]);
  });

  it("removal_without_the_accounts_list_falls_back_to_providers", () => {
    const list = snapshot([row("opencode", "b-opencode", 400), row("claude", undefined, 10)]);
    list.applier.applyEvents([
      { event: ev("limits_providers_changed", { providers: ["claude"] }, "2"), overlap: false },
    ]);
    expect(list.limits.map((item) => item.provider)).toEqual(["claude"]);
  });

  it("a_new_snapshot_replaces_both_rows_and_machine_lists_arrive_with_events", () => {
    const list = snapshot([row("opencode", "owner", 100)]);
    list.applier.applySnapshot(
      { items: [], groups: [], counts: {}, limits: [row("opencode", "owner", 120), row("opencode", "b-opencode", 410)], presets: [], truncated: false },
      "3",
    );
    expect(list.limits).toHaveLength(2);
    list.applier.applyEvents([
      limitsChanged("opencode", "b-opencode", 420, "4", {
        machine_credentials: [{ machine_id: "m1", machine_name: "machine-b", expires_at: "2026-12-30T00:00:00.000Z" }],
        machine_credits: [],
      }),
    ]);
    const other = list.limits.find((item) => item.account === "b-opencode")!;
    expect(other.machine_credentials).toEqual([
      { machine_id: "m1", machine_name: "machine-b", expires_at: "2026-12-30T00:00:00.000Z" },
    ]);
    expect(list.limits.find((item) => item.account === "owner")?.machine_credentials).toBeUndefined();
  });
});

describe("account label on the strip", () => {
  const rows = (accounts: [string, string | undefined][]) =>
    accounts.map(([provider, account]) => row(provider, account, 100));

  it("one_account_per_provider_has_no_label", () => {
    const strip = limitStrip(rows([["claude", undefined], ["opencode", "owner"]]), NOW, DEFAULT_LIMIT_THRESHOLDS);
    expect(strip.rows.map((item) => item.label)).toEqual(["Claude", "OpenCode"]);
  });

  it("a_single_non_owner_account_has_no_label_either", () => {
    const strip = limitStrip(rows([["opencode", "b-opencode"]]), NOW, DEFAULT_LIMIT_THRESHOLDS);
    expect(strip.rows.map((item) => item.label)).toEqual(["OpenCode"]);
  });

  it("two_accounts_of_one_provider_are_labelled_and_the_owner_comes_first", () => {
    const strip = limitStrip(
      rows([["opencode", "b-opencode"], ["claude", undefined], ["opencode", undefined]]),
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(strip.rows.map((item) => item.label)).toEqual(["Claude", "OpenCode · owner", "OpenCode · b-opencode"]);
    expect(new Set(strip.rows.map((item) => item.key)).size).toBe(3);
  });
});

describe("credentials per machine", () => {
  const expires = "2026-12-30T00:00:00.000Z";
  const base = (extra: Partial<ProviderLimits>): ProviderLimits => ({ ...row("claude", undefined, 100), ...extra });

  it("with_one_machine_the_line_is_the_same_as_the_aggregate_one", () => {
    const aggregate = limitStrip([base({ credential_expires_at: expires })], NOW, DEFAULT_LIMIT_THRESHOLDS).rows[0]!;
    const single = limitStrip(
      [base({ credential_expires_at: expires, machine_credentials: [{ machine_id: "m1", machine_name: "machine-a", expires_at: expires }] })],
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    ).rows[0]!;
    expect(single.credentials).toEqual(aggregate.credentials);
    expect(single.credentials[0]!.text).toBe("Вход Claude до 30.12");
  });

  it("with_several_machines_each_line_names_its_machine", () => {
    const strip = limitStrip(
      [
        base({
          credential_expires_at: "2026-11-22T00:00:00.000Z",
          machine_credentials: [
            { machine_id: "m1", machine_name: "machine-a", expires_at: "2026-11-22T00:00:00.000Z" },
            { machine_id: "m2", machine_name: "machine-b", expires_at: expires },
            { machine_id: "m3", machine_name: "machine-c", expires_at: null },
          ],
        }),
      ],
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(strip.rows[0]!.credentials.map((line) => line.text)).toEqual([
      "Вход Claude (machine-a) заканчивается 22.11 — войдите заново",
      "Вход Claude (machine-b) до 30.12",
    ]);
  });

  it("an_account_label_and_a_machine_name_both_show_when_both_apply", () => {
    const strip = limitStrip(
      [
        { ...row("opencode", "owner", 100) },
        { ...row("opencode", "b-opencode", 100), machine_credentials: [{ machine_id: "m2", machine_name: "machine-b", expires_at: expires }] },
      ],
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(strip.rows[1]!.credentials.map((line) => line.text)).toEqual(["Вход OpenCode · b-opencode до 30.12"]);
  });

  it("credits_of_several_machines_are_listed_per_machine", () => {
    const credits = (count: number, machine_name: string) => ({
      reset_count: count,
      resets: [{ title: null, expires_at: "2026-12-30T00:00:00.000Z" }],
      observed_at: AT,
      machine_id: machine_name,
      machine_name,
    });
    const strip = limitStrip(
      [{ ...row("codex", undefined, 100), machine_credits: [credits(1, "machine-a"), credits(2, "machine-b")] }],
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(strip.rows[0]!.creditLines.map((line) => line.text)).toEqual([
      "Codex (machine-a): сбросы лимитов 1, ближайший сгорит 30.12",
      "Codex (machine-b): сбросы лимитов 2, ближайший сгорит 30.12",
    ]);
    expect(strip.rows[0]!.creditList.map((entry) => entry.machine)).toEqual(["machine-a", "machine-b"]);
  });
});
