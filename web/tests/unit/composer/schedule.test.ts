import { describe, it, expect } from "vitest";
import { presets, validateScheduleTime } from "#lib/composer/schedule.js";

describe("composer/schedule", () => {
  it("schedule_validation_follows_the_hub_window", () => {
    const now = 1_700_000_000_000;

    // 1.3a rule: now + 1 s <= run_at <= now + 366 d
    // Below 1 second -> too_soon
    expect(validateScheduleTime(now, now)).toBe("too_soon");
    expect(validateScheduleTime(now + 500, now)).toBe("too_soon");
    expect(validateScheduleTime(now + 999, now)).toBe("too_soon");
    expect(validateScheduleTime(now - 10_000, now)).toBe("too_soon");

    // Exactly now + 1 s -> ok
    expect(validateScheduleTime(now + 1_000, now)).toBe("ok");

    // In the middle of the window -> ok
    expect(validateScheduleTime(now + 3600_000, now)).toBe("ok");
    expect(validateScheduleTime(now + 30 * 86_400_000, now)).toBe("ok");

    // Exactly now + 366 days -> ok
    const maxAllowed = now + 366 * 86_400_000;
    expect(validateScheduleTime(maxAllowed, now)).toBe("ok");

    // Beyond now + 366 days -> too_far
    expect(validateScheduleTime(maxAllowed + 1, now)).toBe("too_far");
    expect(validateScheduleTime(maxAllowed + 1_000, now)).toBe("too_far");
  });

  it("presets_are_computed_from_server_corrected_now", () => {
    const now = new Date("2026-10-29T10:00:00Z").getTime();
    const list = presets(now);

    expect(list.map((item) => item.label)).toEqual([
      "Через 15 минут",
      "Через час",
    ]);
    expect(list[0]!.at).toBe(now + 15 * 60 * 1000);
    expect(list[1]!.at).toBe(now + 60 * 60 * 1000);
    expect(list.some((item) => /вечером|утром/i.test(item.label))).toBe(false);
  });
});
