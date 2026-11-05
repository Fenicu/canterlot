import { describe, expect, it } from "vitest";
import { limitsSubtitle, speedRows, usageRows } from "#lib/screens/limits/model.js";
import type { components } from "#lib/api/generated/openapi.js";

type Usage = components["schemas"]["UsageDay"];
const usage = (patch: Partial<Usage>): Usage => ({
  day: "2026-10-29",
  conversation_id: "abcdef12-0000",
  provider: "claude",
  agent: "main",
  model: "opus",
  input_tokens: 100,
  output_tokens: 10,
  cache_read_tokens: 20,
  cache_write_tokens: 30,
  weekly_delta_permille: 20,
  overlapped_runs: 0,
  runs: 1,
  ...patch,
});

describe("screens/limits", () => {
  it("subtitle_names_every_provider_the_page_shows", () => {
    expect(limitsSubtitle([])).toBe("");
    expect(limitsSubtitle([{ provider: "claude" }])).toBe("Claude");
    expect(limitsSubtitle([{ provider: "claude" }, { provider: "codex" }])).toBe(
      "Claude и Codex",
    );
    expect(
      limitsSubtitle([
        { provider: "claude" },
        { provider: "codex" },
        { provider: "opencode" },
      ]),
    ).toBe("Claude, Codex и OpenCode");
    expect(
      limitsSubtitle([
        { provider: "claude" },
        { provider: "codex" },
        { provider: "antigravity" },
      ]),
    ).toBe("Claude, Codex и Antigravity");
  });

  it("subtitle_does_not_repeat_a_provider_twice", () => {
    expect(
      limitsSubtitle([{ provider: "codex" }, { provider: "codex" }]),
    ).toBe("Codex");
  });

  it("speed_rows_put_the_fastest_model_first_and_hide_models_without_numbers", () => {
    const empty = { runs: 0, tokens_per_second: null, first_token_ms: null };
    const rows = speedRows([
      { provider: "claude", model: "claude-opus-5-5", today: { runs: 2, tokens_per_second: 68.4, first_token_ms: 2100 }, period: { runs: 9, tokens_per_second: 71.2, first_token_ms: 1900 } },
      { provider: "opencode", model: "beam", today: empty, period: { runs: 3, tokens_per_second: 1240, first_token_ms: 420 } },
      { provider: "claude", model: "claude-haiku-4-5", today: empty, period: empty },
    ]);
    expect(rows.map((row) => row.model)).toEqual(["beam", "Opus 5.5"]);
    expect(rows[0]?.today).toEqual({ speed: null, firstToken: null });
    expect(rows[0]?.period).toEqual({ speed: "1 240 ток/с", firstToken: "старт 0,4 с" });
    expect(rows[1]?.provider).toBe("Claude");
  });

  it("usage_rows_put_providers_without_a_weekly_window_after_rows_with_a_number", () => {
    const rows = usageRows([
      usage({ provider: "opencode", model: "free-large", weekly_delta_permille: 50, input_tokens: 50_000, output_tokens: 5_000 }),
      usage({ provider: "claude", model: "opus", weekly_delta_permille: 10, input_tokens: 1_000, output_tokens: 100 }),
      usage({ provider: "antigravity", model: "gemini", weekly_delta_permille: 2, input_tokens: 2_000, output_tokens: 200 }),
      usage({ provider: "opencode", model: "free-small", weekly_delta_permille: 1, input_tokens: 900, output_tokens: 100 }),
    ], "2026-10-29", new Map(), new Set(["claude", "antigravity"]));

    expect(rows.map((row) => row.detail.split(" · ").at(-1))).toEqual(["opus", "gemini", "free-large", "free-small"]);
    expect(rows.map((row) => row.weekly)).toEqual([true, true, false, false]);
    expect(rows.map((row) => row.delta)).toEqual([10, 2, 50, 1]);
  });
});

import { currentBlocks, historyCharts } from "#lib/screens/limits/model.js";
import type { ProviderLimits } from "#lib/stores/limits.js";

describe("screens/limits current blocks", () => {
  const NOW = new Date(2026, 9, 29, 12, 0).getTime();
  const cfg = { stale_after_secs: 35 * 60, warn_pcts: [80, 95], weekly_reserve_pct: 95, warn_remaining_pct: 20, danger_remaining_pct: 5, reserve_remaining_pct: 5 };
  const observed = new Date(NOW - 60_000).toISOString();
  const win = (used: number, reset = new Date(2026, 9, 30, 18, 34).toISOString(), at = observed) => ({ used_permille: used, resets_at: reset, observed_at: at });

  it("rows_exist_only_for_windows_the_provider_has_and_say_remaining_explicitly", () => {
    const providers: ProviderLimits[] = [
      { provider: "opencode", weekly_reserve_active: false, five_hour: win(125), weekly: win(340), monthly: win(93) },
      { provider: "codex", weekly_reserve_active: false, five_hour_absent: true, weekly: win(420) },
      { provider: "claude", weekly_reserve_active: false, five_hour: win(610), weekly: win(845) },
      { provider: "antigravity", weekly_reserve_active: false, five_hour: win(260), weekly: win(410) },
    ];
    const blocks = currentBlocks(providers, NOW, cfg);
    expect(blocks.map((b) => b.label)).toEqual(["Claude", "Codex", "OpenCode", "Antigravity"]);
    expect(blocks[0]!.windows.map((w) => [w.label, w.value, w.reset])).toEqual([
      ["5 часов", "осталось 39%", "сброс пт 18:34"],
      ["неделя", "осталось 16%", "сброс пт 18:34"],
    ]);
    expect(blocks[0]!.windows[1]!.tone).toBe("warning");
    expect(blocks[1]!.windows.map((w) => w.key)).toEqual(["week"]);
    expect(blocks[2]!.windows.map((w) => w.label)).toEqual(["5 часов", "неделя", "месяц"]);
    expect(blocks[2]!.windows[2]!.value).toBe("осталось 91%");
    expect(blocks[3]!.windows.map((w) => w.value)).toEqual(["осталось 74%", "осталось 59%"]);
  });

  it("missing_windows_are_not_drawn_and_a_provider_without_any_says_no_data", () => {
    const blocks = currentBlocks([{ provider: "claude", weekly_reserve_active: false, five_hour: null, weekly: win(100) }, { provider: "codex", weekly_reserve_active: false }], NOW, cfg);
    expect(blocks[0]!.windows.map((w) => w.key)).toEqual(["week"]);
    expect(blocks[1]!.windows).toEqual([]);
    expect(blocks[1]!.empty).toBe(true);
  });

  it("estimated_stale_and_reserve_are_quiet_markers", () => {
    const old = new Date(NOW - 3 * 3600_000).toISOString();
    const [block] = currentBlocks([{ provider: "claude", weekly_reserve_active: true, data_confidence: "estimated", five_hour: win(200, undefined, old), weekly: win(960) }], NOW, cfg);
    expect(block!.markers).toEqual(["оценка", "резерв недели"]);
    expect(block!.windows[0]).toMatchObject({ stale: true, value: "устарело", reset: "" });
    expect(block!.windows[1]!.tone).toBe("danger");
  });
});

describe("screens/limits history charts", () => {
  const zone = "UTC";
  const days = ["2026-10-28", "2026-10-29", "2026-10-30"];
  const point = (day: string, hour: number, used: number) => ({ observed_at: `${day}T${String(hour).padStart(2, "0")}:00:00Z`, used_permille: used, used_pct: used / 10 });

  it("one_chart_per_provider_with_weekly_points_using_the_last_point_of_each_day", () => {
    const charts = historyCharts([
      { provider: "codex", five_hour: [], weekly: [point("2026-10-28", 10, 300), point("2026-10-30", 9, 500)] },
      { provider: "opencode", five_hour: [point("2026-10-29", 1, 100)], weekly: [] },
      { provider: "claude", five_hour: [point("2026-10-28", 1, 900)], weekly: [point("2026-10-28", 23, 711), point("2026-10-28", 3, 100), point("2026-10-29", 1, 800), point("2026-10-30", 1, 995)] },
    ], days, zone);
    expect(charts.map((c) => c.label)).toEqual(["Claude", "Codex"]);
    expect(charts[0]!.days.map((d) => [d.mark, d.value === null ? null : Math.round(d.value * 10) / 10, d.text])).toEqual([["28", 28.9, "29%"], ["29", 20, "20%"], ["30", 0.5, "<1%"]]);
    expect(charts[0]!.today).toBe("<1%");
    expect(charts[1]!.days.map((d) => d.text)).toEqual(["70%", "", "50%"]);
    expect(charts[1]!.summary).toBe("Codex: от 50 до 70%, сегодня 50%");
  });

  it("a_provider_appears_once_even_if_series_repeat", () => {
    const series = { provider: "claude", five_hour: [], weekly: [point("2026-10-29", 1, 500)] };
    const charts = historyCharts([series, series], days, zone);
    expect(charts).toHaveLength(1);
    expect(charts[0]!.today).toBe("");
    expect(charts[0]!.summary).toBe("Claude: от 50 до 50%, сегодня нет данных");
  });
});
