import { describe, expect, it } from "vitest";
import {
  CRON_PRESETS,
  cronHuman,
  defaultTimezone,
  fireTime,
  outcomeWord,
  parseCron,
  runDetails,
  targetLabel
} from "#lib/screens/settings/schedules.js";

describe("cron presets and human text", () => {
  it("presets stay consistent with the parser", () => {
    expect(CRON_PRESETS.map((item) => item.cron)).toEqual(["0 9 * * *", "0 9 * * 1-5", "0 * * * *"]);
    for (const preset of CRON_PRESETS) expect(parseCron(preset.cron)).not.toBeNull();
    expect(cronHuman("0 9 * * *")).toBe("Каждый день в 09:00");
    expect(cronHuman("0 9 * * 1-5")).toBe("По будням в 09:00");
    expect(cronHuman("0 * * * *")).toBe("Каждый час");
  });

  it("describes steps, day sets and short hour lists", () => {
    expect(cronHuman("*/15 * * * *")).toBe("Каждые 15 мин.");
    expect(cronHuman("* * * * *")).toBe("Каждую минуту");
    expect(cronHuman("30 8 * * 1-5")).toBe("По будням в 08:30");
    expect(cronHuman("0 9 * * 0,6")).toBe("По выходным в 09:00");
    expect(cronHuman("0 10,12 * * *")).toBe("Каждый день в 10:00, 12:00");
    expect(cronHuman("0 1 * * 1")).toBe("По понедельникам в 01:00");
  });

  it("refuses malformed and out-of-range expressions", () => {
    for (const cron of ["", "0 9 * *", "60 9 * * *", "0 25 * * *", "*/0 * * * *", "0 9 * * 8", "a b c d e", "0 9 * * 5-1"]) {
      expect(parseCron(cron), cron).toBeNull();
      expect(cronHuman(cron), cron).toBe("Некорректное расписание");
    }
  });
});

describe("outcome wording", () => {
  it("uses words and tones, never glyphs", () => {
    expect(outcomeWord("started")).toEqual({ word: "Запущено", tone: "ok" });
    expect(outcomeWord("failed")).toEqual({ word: "Ошибка", tone: "danger" });
    expect(outcomeWord("claimed")).toEqual({ word: "Заявка", tone: "neutral" });
    expect(outcomeWord("skipped_limits")).toEqual({ word: "Пропущено: лимиты", tone: "warn" });
    expect(outcomeWord("skipped_precheck")).toEqual({ word: "Пропущено: предпроверка", tone: "warn" });
    expect(outcomeWord("missed")).toEqual({ word: "Пропущено: время ушло", tone: "warn" });
    for (const state of ["claimed", "prechecking", "started", "skipped_precheck", "skipped_limits", "skipped_overlap", "skipped_machine_offline", "skipped_changed", "missed", "failed"] as const) {
      const { word } = outcomeWord(state);
      expect(word).not.toMatch(/[\u2713\u2717\u2718\u26a0\u2705\u274c\u2b50]/u);
    }
  });
});

describe("run detail and times", () => {
  it("reads known detail keys and tolerates unknown ones", () => {
    expect(runDetails({
      precheck_exit_code: 7,
      precheck_tail: "нет изменений",
      limits_window: "five_hour",
      missed_before: 3,
      run_anyway: true,
      something_new: 1
    })).toEqual({
      code: 7, tail: "нет изменений", window: "five_hour", missedBefore: 3, runAnyway: true, reason: "", reportText: ""
    });
    expect(runDetails(null)).toEqual({ code: null, tail: "", window: "", missedBefore: 0, runAnyway: false, reason: "", reportText: "" });
    expect(runDetails({ exit_code: 0, missed_before: -2, reason: "не готово" }).reason).toBe("не готово");
  });

  it("renders fire times in the requested zone and survives a bad name", () => {
    const iso = "2026-11-18T09:00:00.000Z";
    expect(fireTime(iso, "UTC")).toMatch(/18 нояб.*09:00/u);
    expect(fireTime(iso, "Europe/Moscow")).toMatch(/18 нояб.*12:00/u);
    expect(fireTime(iso, "Not/AZone")).not.toBe("—");
    expect(fireTime("bad")).toBe("—");
  });
});

describe("defaultTimezone", () => {
  it("prefers the owner zone, then the browser zone, then Moscow", () => {
    expect(defaultTimezone("Asia/Yekaterinburg", "Europe/Berlin")).toBe("Asia/Yekaterinburg");
    expect(defaultTimezone(null, "Europe/Berlin")).toBe("Europe/Berlin");
    expect(defaultTimezone("", "")).toBe("Europe/Moscow");
    expect(defaultTimezone(undefined, undefined)).toBe("Europe/Moscow");
  });

  it("skips zones the runtime cannot resolve", () => {
    expect(defaultTimezone("Not/AZone", "Europe/Berlin")).toBe("Europe/Berlin");
    expect(defaultTimezone("Not/AZone", "Also/Bad")).toBe("Europe/Moscow");
  });
});

describe("session_report target", () => {
  const sessions = new Map([["s1", { title: "Работа" }]]);

  it("names the target kind and the session", () => {
    expect(targetLabel({ kind: "session_report", conversation_id: "s1" }, sessions)).toEqual({ kind: "Отчёт из сессии", session: "Работа" });
    expect(targetLabel({ kind: "existing_session", conversation_id: "s1" }, sessions)).toEqual({ kind: "Сообщение в сессию", session: "Работа" });
    expect(targetLabel({ kind: "new_session", machine_id: "m", workdir: "/w" }, sessions)).toEqual({ kind: "Новая сессия", session: null });
  });

  it("falls back to a neutral name when the session is not in the list", () => {
    expect(targetLabel({ kind: "session_report", conversation_id: "gone" }, sessions).session).toBe("Сессия недоступна");
  });

  it("reads the stored report text from a run detail", () => {
    expect(runDetails({ report: { state: "ready", text: "## Итоги" } }).reportText).toBe("## Итоги");
    expect(runDetails({ report: { state: "failed" } }).reportText).toBe("");
    expect(runDetails({}).reportText).toBe("");
  });
});
