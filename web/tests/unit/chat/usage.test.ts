import { describe, expect, test } from "vitest";
import { summarizeUsage } from "#lib/screens/chat/usage.js";
import type { components } from "#lib/api/generated/openapi.js";

type SessionUsage = components["schemas"]["SessionUsageView"];
type Totals = components["schemas"]["UsageTotalsView"];

function totals(over: Partial<Totals> = {}): Totals {
  return {
    turns: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    models: [],
    ...over,
  };
}

describe("summarizeUsage", () => {
  test("shows compact session totals with exact values and both cache fields", () => {
    const usage: SessionUsage = {
      session: totals({
        turns: 7,
        input_tokens: 1_200_000,
        output_tokens: 345_000,
        cache_read_tokens: 12_345_678,
        cache_write_tokens: 1_500,
        models: [
          {
            model: "opus-5.5",
            input_tokens: 1_100_000,
            output_tokens: 300_000,
            cache_read_tokens: 12_000_000,
            cache_write_tokens: 1_500,
          },
          {
            model: "haiku",
            input_tokens: 100_000,
            output_tokens: 45_000,
            cache_read_tokens: 345_678,
            cache_write_tokens: 0,
          },
        ],
      }),
      delegates: totals(),
    };

    const summary = summarizeUsage(usage);
    expect(summary.session.rows).toEqual([
      { label: "Вход", value: { compact: "1,2 млн", exact: "1 200 000" } },
      { label: "Выход", value: { compact: "345 тыс.", exact: "345 000" } },
      {
        label: "Прочитано из кэша",
        value: { compact: "12,3 млн", exact: "12 345 678" },
      },
      { label: "Записано в кэш", value: { compact: "1,5 тыс.", exact: "1 500" } },
      { label: "Ходов", value: { compact: "7", exact: "7" } },
    ]);
    expect(summary.session.showCacheRead).toBe(true);
    expect(summary.session.showCacheWrite).toBe(true);
    expect(summary.session.models[0]).toEqual({
      model: "opus-5.5",
      total: { compact: "13,4 млн", exact: "13 401 500" },
      input: { compact: "1,1 млн", exact: "1 100 000" },
      output: { compact: "300 тыс.", exact: "300 000" },
      cacheRead: { compact: "12 млн", exact: "12 000 000" },
      cacheWrite: { compact: "1,5 тыс.", exact: "1 500" },
    });
    expect(summary.hasDelegates).toBe(false);
  });

  test("renders billions and exact turn counts for a long session", () => {
    const summary = summarizeUsage({
      session: totals({
        turns: 1_024,
        input_tokens: 1_200_000,
        output_tokens: 88_300_000,
        cache_read_tokens: 27_952_400_000,
        cache_write_tokens: 942_200_000,
        models: [
          {
            model: "claude-opus-5-5",
            input_tokens: 237_900,
            output_tokens: 61_200_000,
            cache_read_tokens: 21_558_400_000,
            cache_write_tokens: 484_600_000,
          },
          {
            model: "claude-sonnet-5-5",
            input_tokens: 76_300,
            output_tokens: 26_700_000,
            cache_read_tokens: 6_334_200_000,
            cache_write_tokens: 452_400_000,
          },
          {
            model: "claude-fable-5-2",
            input_tokens: 2_600,
            output_tokens: 239_900,
            cache_read_tokens: 59_700_000,
            cache_write_tokens: 4_700_000,
          },
          {
            model: "claude-haiku-5-5",
            input_tokens: 838_800,
            output_tokens: 153_300,
            cache_read_tokens: 0,
            cache_write_tokens: 499_300,
          },
        ],
      }),
      delegates: totals({
        turns: 408,
        input_tokens: 56_500_000,
        output_tokens: 21_700_000,
        cache_read_tokens: 6_527_100_000,
        cache_write_tokens: 28_700_000,
      }),
    });

    expect(summary.session.rows).toEqual([
      { label: "Вход", value: { compact: "1,2 млн", exact: "1 200 000" } },
      { label: "Выход", value: { compact: "88,3 млн", exact: "88 300 000" } },
      {
        label: "Прочитано из кэша",
        value: { compact: "28 млрд", exact: "27 952 400 000" },
      },
      {
        label: "Записано в кэш",
        value: { compact: "942,2 млн", exact: "942 200 000" },
      },
      { label: "Ходов", value: { compact: "1 024", exact: "1 024" } },
    ]);

    expect(summary.session.models.map((row) => row.input.compact)).toEqual([
      "237,9 тыс.",
      "76,3 тыс.",
      "2,6 тыс.",
      "838,8 тыс.",
    ]);
    expect(summary.session.models.map((row) => row.output.compact)).toEqual([
      "61,2 млн",
      "26,7 млн",
      "239,9 тыс.",
      "153,3 тыс.",
    ]);
    expect(summary.session.models.map((row) => row.cacheRead.compact)).toEqual([
      "21,6 млрд",
      "6,33 млрд",
      "59,7 млн",
      "0",
    ]);
    expect(summary.session.models.map((row) => row.cacheWrite.compact)).toEqual([
      "484,6 млн",
      "452,4 млн",
      "4,7 млн",
      "499,3 тыс.",
    ]);

    expect(summary.delegates.rows).toEqual([
      { label: "Вход", value: { compact: "56,5 млн", exact: "56 500 000" } },
      { label: "Выход", value: { compact: "21,7 млн", exact: "21 700 000" } },
      {
        label: "Прочитано из кэша",
        value: { compact: "6,53 млрд", exact: "6 527 100 000" },
      },
      {
        label: "Записано в кэш",
        value: { compact: "28,7 млн", exact: "28 700 000" },
      },
      { label: "Ходов", value: { compact: "408", exact: "408" } },
    ]);
  });

  test("hides cache rows when the harness reports no cache", () => {
    const summary = summarizeUsage({
      session: totals({ turns: 1, input_tokens: 10, output_tokens: 2 }),
      delegates: totals(),
    });
    expect(summary.session.rows.map((row) => row.label)).toEqual([
      "Вход",
      "Выход",
      "Ходов",
    ]);
    expect(summary.session.showCacheRead).toBe(false);
    expect(summary.session.showCacheWrite).toBe(false);
    expect(summary.session.empty).toBe(false);
    expect(summary.delegates.empty).toBe(true);
  });

  test("delegates keep their own totals and per-model breakdown", () => {
    const summary = summarizeUsage({
      session: totals({ turns: 2, input_tokens: 100 }),
      delegates: totals({
        turns: 4,
        input_tokens: 900_000,
        output_tokens: 50_000,
        models: [
          {
            model: "sonnet-5",
            input_tokens: 900_000,
            output_tokens: 50_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
          },
        ],
      }),
    });

    expect(summary.hasDelegates).toBe(true);
    expect(summary.delegates.rows).toEqual([
      { label: "Вход", value: { compact: "900 тыс.", exact: "900 000" } },
      { label: "Выход", value: { compact: "50 тыс.", exact: "50 000" } },
      { label: "Ходов", value: { compact: "4", exact: "4" } },
    ]);
    expect(summary.delegates.models[0]?.model).toBe("sonnet-5");
    expect(summary.delegates.models[0]?.input.compact).toBe("900 тыс.");
    expect(summary.session.models).toEqual([]);
  });
});
