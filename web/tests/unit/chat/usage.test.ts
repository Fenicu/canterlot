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
