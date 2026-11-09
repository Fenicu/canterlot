import { describe, expect, test } from "vitest";
import { summarizeRun } from "#lib/screens/chat/info.js";
import type { RunInfo } from "#lib/stores/run.js";

describe("summarizeRun", () => {
  test("summarize_run_matches_mockup_04_rows", () => {
    const info: RunInfo = {
      run_id: "00000000-0000-4000-8000-000000000001",
      input_id: "00000000-0000-4000-8000-000000000002",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: "2026-10-26T22:14:18.000Z",
      duration_ms: 138000,
      context: { tokens: 120000, window_tokens: 200000 },
      limit: { provider: "claude", weekly_delta_permille: 4, exact: true },
      parts: [
        {
          block_id: "b-1",
          kind: "main",
          agent: "Claude Code",
          model: "opus-5.5",
        },
        {
          block_id: "b-2",
          kind: "subagent",
          agent: "Explore",
          model: "haiku",
          task: "Поиск дедупликации",
        },
      ],
      usage: [
        {
          agent: "Claude Code",
          model: "opus-5.5",
          input_tokens: 18420,
          output_tokens: 1842,
          cache_read_tokens: 14080,
          cache_write_tokens: 2128,
        },
        {
          agent: "Explore",
          model: "haiku",
          input_tokens: 4216,
          output_tokens: 386,
          cache_read_tokens: 3104,
          cache_write_tokens: 0,
        },
      ],
    };

    const summary = summarizeRun(info);

    expect(summary.pending).toBe(false);
    expect(summary.duration).toBe("2 мин 18 с");
    expect(summary.context).toEqual({ used: 120000, window: 200000 });
    expect(summary.limit).toEqual({
      text: "≈ 0,4% недели",
      note: "Оценка по изменению квоты",
    });

    expect(summary.parts).toEqual([
      { title: "Основной ответ", sub: "Claude Code", model: "opus-5.5" },
      {
        title: "Поиск дедупликации",
        sub: "Субагент · Explore",
        model: "Haiku",
      },
    ]);

    expect(summary.tokens).toEqual([
      { label: "Вход", main: 18420, sub: 4216 },
      { label: "Выход", main: 1842, sub: 386 },
      { label: "Прочитано из кэша", main: 14080, sub: 3104 },
      { label: "Записано в кэш", main: 2128, sub: 0 },
    ]);
  });

  test("summarize_run_states_overlap_honestly_when_not_exact", () => {
    const info: RunInfo = {
      run_id: "00000000-0000-4000-8000-000000000003",
      input_id: "00000000-0000-4000-8000-000000000004",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: "2026-10-26T22:13:00.000Z",
      duration_ms: 60000,
      limit: { provider: "claude", weekly_delta_permille: 10, exact: false },
      parts: [
        { block_id: "b-1", kind: "main", agent: "Claude Code", model: "opus" },
      ],
      usage: [
        {
          agent: "Claude Code",
          model: "opus",
          input_tokens: 1000,
          output_tokens: 100,
          cache_read_tokens: 0,
          cache_write_tokens: 0,
        },
      ],
    };

    const summary = summarizeRun(info);
    expect(summary.limit?.note).toBe(
      "Оценка приблизительная",
    );
    expect(summary.limit?.text).toBe("≈ 1,0% недели");
  });

  test("summarize_run_for_a_running_turn_shows_pending_totals", () => {
    const info: RunInfo = {
      run_id: "00000000-0000-4000-8000-000000000005",
      input_id: "00000000-0000-4000-8000-000000000006",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: null,
      duration_ms: null,
      parts: [
        { block_id: "b-1", kind: "main", agent: "Claude Code", model: "opus" },
      ],
      usage: [],
    };

    const summary = summarizeRun(info);
    expect(summary.pending).toBe(true);
    expect(summary.duration).toBeNull();
  });

  test("hub_main_agent_is_counted_as_main_and_repeated_parts_collapse", () => {
    const info = {
      run_id: "00000000-0000-4000-8000-000000000011",
      input_id: "00000000-0000-4000-8000-000000000012",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: "2026-10-26T22:12:05.000Z",
      duration_ms: 5000,
      usage: [
        { agent: "main", model: "synthetic-large", input_tokens: 12000, output_tokens: 3400, cache_read_tokens: 0, cache_write_tokens: 0 },
        { agent: "Explore", model: "synthetic-small", input_tokens: 8000, output_tokens: 900, cache_read_tokens: 0, cache_write_tokens: 0 },
      ],
      // Хаб отдаёт часть на каждый блок: у основного агента их несколько, имя по умолчанию «main».
      parts: [
        { block_id: "a", kind: "text", agent: "main", model: "synthetic-large" },
        { block_id: "b", kind: "tool_call", agent: "main", model: "synthetic-large" },
        { block_id: "c", kind: "text", agent: "Explore", model: "synthetic-small" },
        { block_id: "d", kind: "tool_call", agent: "Explore", model: "synthetic-small" },
      ],
    } as unknown as RunInfo;
    const model = summarizeRun(info);
    expect(model.parts.map((part) => `${part.title}|${part.model}`)).toEqual(["Основной ответ|synthetic-large", "Explore|synthetic-small"]);
    expect(model.tokens.find((row) => row.label === "Вход")).toMatchObject({ main: 12000, sub: 8000 });
    expect(model.tokens.find((row) => row.label === "Выход")).toMatchObject({ main: 3400, sub: 900 });
  });

  test("a_weekly_share_below_one_tenth_is_named_not_rounded_to_zero", () => {
    const info = {
      run_id: "00000000-0000-4000-8000-000000000021",
      input_id: "00000000-0000-4000-8000-000000000022",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: "2026-10-26T22:12:03.000Z",
      duration_ms: 3000,
      limit: { provider: "claude", weekly_delta_permille: 0, exact: true },
      parts: [],
      usage: [],
    } as unknown as RunInfo;
    expect(summarizeRun(info).limit?.text).toBe("меньше 0,1% недели");
  });

  test("main_part_names_the_concrete_version_with_the_session_1m_variant", () => {
    const info = {
      run_id: "00000000-0000-4000-8000-000000000031",
      input_id: "00000000-0000-4000-8000-000000000032",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: "2026-10-26T22:12:03.000Z",
      duration_ms: 3000,
      parts: [
        { block_id: "a", kind: "text", agent: "main", model: "claude-opus-5-5" },
        { block_id: "b", kind: "text", agent: "Explore", model: "claude-opus-5-5" },
        { block_id: "c", kind: "text", agent: "Other", model: "claude-haiku-4-5" },
      ],
      usage: [],
    } as unknown as RunInfo;
    const parts = summarizeRun(info, null, "opus[1m]").parts;
    expect(parts.map((part) => part.model)).toEqual(["Opus 5.5 1M", "Opus 5.5", "Haiku 4.5"]);
    expect(summarizeRun(info).parts[0]?.model).toBe("Opus 5.5");
  });

  test("speed_line_shows_generation_and_first_token_and_names_models_only_when_several", () => {
    const base = {
      run_id: "00000000-0000-4000-8000-000000000041",
      input_id: "00000000-0000-4000-8000-000000000042",
      started_at: "2026-10-26T22:12:00.000Z",
      finished_at: "2026-10-26T22:12:03.000Z",
      duration_ms: 3000,
      parts: [],
      usage: [],
    };
    const one = { ...base, speed: [{ model: "opus", output_tokens: 400, generation_ms: 2817, tokens_per_second: 142, first_token_ms: 1800 }] } as RunInfo;
    expect(summarizeRun(one).speed).toEqual(["генерация 142 ток/с · первый токен 1,8 с"]);
    const two = {
      ...base,
      speed: [
        { model: "claude-opus-5-5", output_tokens: 400, tokens_per_second: 80, first_token_ms: null },
        { model: "claude-haiku-4-5", output_tokens: 10, tokens_per_second: null, first_token_ms: 600 },
      ],
    } as RunInfo;
    expect(summarizeRun(two).speed).toEqual(["Opus 5.5: генерация 80 ток/с", "Haiku 4.5: первый токен 0,6 с"]);
    expect(summarizeRun({ ...base } as RunInfo).speed).toEqual([]);
  });
});
