import { describe, expect, it } from "vitest";
import {
  alignCharts,
  currentBlocks,
  historyCharts,
  providersWithWeekly,
  usageGroups,
  usageRows,
} from "#lib/screens/limits/model.js";
import { DEFAULT_LIMIT_THRESHOLDS, type ProviderLimits } from "#lib/stores/limits.js";
import type { components } from "#lib/api/generated/openapi.js";

type Usage = components["schemas"]["UsageDay"];

const NOW = Date.parse("2026-11-20T10:01:00.000Z");
const AT = "2026-11-20T10:00:00.000Z";
const win = (used: number) => ({ used_permille: used, resets_at: "2026-11-25T10:00:00.000Z", observed_at: AT });
const row = (provider: string, account: string | undefined, used: number): ProviderLimits => ({
  provider,
  ...(account ? { account } : {}),
  weekly: win(used),
  weekly_reserve_active: false,
});
const usage = (patch: Partial<Usage>): Usage => ({
  day: "2026-11-20",
  conversation_id: "abcdef12-0000",
  provider: "opencode",
  account: "owner",
  agent: "main",
  model: "beam",
  input_tokens: 100,
  output_tokens: 10,
  cache_read_tokens: 0,
  cache_write_tokens: 0,
  weekly_delta_permille: 20,
  overlapped_runs: 0,
  runs: 1,
  ...patch,
});
const point = (day: string, used: number) => ({ observed_at: `${day}T10:00:00Z`, used_permille: used, used_pct: used / 10 });
const series = (provider: string, account: string, used: number) => ({
  provider,
  account,
  five_hour: [],
  weekly: [point("2026-11-20", used)],
  weekly_fable: [],
});

describe("limits screen with two accounts of one provider", () => {
  const two = [row("opencode", "b-opencode", 400), row("opencode", undefined, 100), row("claude", undefined, 50)];

  it("one_card_per_account_labelled_only_for_a_provider_with_several", () => {
    const blocks = currentBlocks(two, NOW, DEFAULT_LIMIT_THRESHOLDS);
    expect(blocks.map((block) => block.label)).toEqual(["Claude", "OpenCode · owner", "OpenCode · b-opencode"]);
    expect(new Set(blocks.map((block) => block.key)).size).toBe(3);
    const single = currentBlocks([row("opencode", "b-opencode", 400)], NOW, DEFAULT_LIMIT_THRESHOLDS);
    expect(single.map((block) => block.label)).toEqual(["OpenCode"]);
  });

  it("charts_are_one_per_account_and_follow_the_card_order", () => {
    const blocks = currentBlocks(two, NOW, DEFAULT_LIMIT_THRESHOLDS);
    const charts = historyCharts(
      [series("opencode", "b-opencode", 400), series("opencode", "owner", 100), series("claude", "owner", 50)],
      ["2026-11-20"],
      "UTC",
    );
    expect(charts.map((chart) => chart.label)).toEqual(["Claude", "OpenCode · owner", "OpenCode · b-opencode"]);
    const slots = alignCharts(charts.slice().reverse(), blocks);
    expect(slots.map((slot) => slot.label)).toEqual(["Claude", "OpenCode · owner", "OpenCode · b-opencode"]);
  });

  it("a_chart_label_follows_the_limits_rows_when_the_other_account_has_no_snapshots_yet", () => {
    const charts = historyCharts([series("opencode", "b-opencode", 400)], ["2026-11-20"], "UTC", new Set(["opencode"]));
    expect(charts.map((chart) => chart.label)).toEqual(["OpenCode · b-opencode"]);
    expect(historyCharts([series("opencode", "b-opencode", 400)], ["2026-11-20"], "UTC")[0]!.label).toBe("OpenCode");
  });

  it("today_usage_is_grouped_per_account_with_its_own_weekly_flag", () => {
    const weekly = providersWithWeekly([row("opencode", undefined, 100), { provider: "opencode", account: "b-opencode", weekly_reserve_active: false }]);
    const groups = usageGroups(
      usageRows(
        [usage({ weekly_delta_permille: 30 }), usage({ account: "b-opencode", weekly_delta_permille: 70 })],
        "2026-11-20",
        new Map(),
        weekly,
      ),
    );
    expect(groups.map((group) => [group.label, group.weekly, group.total])).toEqual([
      ["OpenCode · owner", true, "≈ 3,0%"],
      ["OpenCode · b-opencode", false, "—"],
    ]);
    expect(new Set(groups.map((group) => group.key)).size).toBe(2);
  });

  it("one_account_groups_look_as_before", () => {
    const groups = usageGroups(usageRows([usage({})], "2026-11-20", new Map(), new Set(["opencode"])));
    expect(groups.map((group) => group.label)).toEqual(["OpenCode"]);
  });
});

describe("limits screen credentials per machine", () => {
  const credits = (name: string) => ({
    reset_count: 1,
    resets: [{ title: null, expires_at: null }],
    observed_at: AT,
    machine_id: name,
    machine_name: name,
  });

  it("several_machines_are_listed_by_name_and_one_machine_looks_as_before", () => {
    const [many] = currentBlocks(
      [
        {
          ...row("codex", undefined, 100),
          machine_credentials: [
            { machine_id: "a", machine_name: "machine-a", expires_at: "2027-01-30T00:00:00.000Z" },
            { machine_id: "b", machine_name: "machine-b", expires_at: "2027-02-27T00:00:00.000Z" },
          ],
          machine_credits: [credits("machine-a"), credits("machine-b")],
        },
      ],
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(many!.credentials.map((line) => line.text)).toEqual([
      "Вход Codex (machine-a) до 30.01",
      "Вход Codex (machine-b) до 27.02",
    ]);
    expect(many!.credits.map((credit) => credit.title)).toEqual(["Сброс лимитов (machine-a)", "Сброс лимитов (machine-b)"]);
    const [one] = currentBlocks(
      [{ ...row("codex", undefined, 100), machine_credentials: [{ machine_id: "a", machine_name: "machine-a", expires_at: "2027-01-30T00:00:00.000Z" }], machine_credits: [credits("machine-a")] }],
      NOW,
      DEFAULT_LIMIT_THRESHOLDS,
    );
    expect(one!.credentials.map((line) => line.text)).toEqual(["Вход Codex до 30.01"]);
    expect(one!.credits.map((credit) => credit.title)).toEqual(["Сброс лимитов"]);
  });
});
