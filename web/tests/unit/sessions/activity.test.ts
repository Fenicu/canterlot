import { describe, expect, it } from "vitest";
import { chatStateLine } from "#lib/screens/chat/stateLine.js";
import {
  activityLine,
  turnRunning,
  type Activity,
} from "#lib/screens/sessions/activityText.js";

const now = Date.parse("2026-10-29T12:00:00.000Z");

function line(activity: Activity) {
  return activityLine(activity, { now });
}

describe("sessions/activity", () => {
  it("a_session_waiting_for_the_heavy_task_queue_says_so_instead_of_the_tool", () => {
    const wait = (kind: string, position: number) =>
      line({
        state: "working",
        evidence: {
          phase: "running_tool",
          tool_name: "Bash",
          summary: "scripts/check.sh --fast",
          lease_wait: { kind, position, since: "2026-10-29T11:50:00.000Z" },
        },
      });
    expect(wait("build", 2)).toEqual({
      text: "Ждёт очереди на сборку · 2-й",
      tone: "accent",
      live: false,
    });
    expect(wait("gate", 1).text).toBe("Ждёт очереди на гейт · 1-й");
  });

  it("activity_line_covers_every_state_and_evidence_with_russian_text_and_tone", () => {
    expect(line({ state: "idle" })).toEqual({
      text: "Новая сессия",
      tone: "default",
      live: false,
    });
    expect(
      line({
        state: "idle",
        evidence: { last_turn_at: "2026-10-29T11:00:00.000Z" },
      }).text,
    ).toBe("Ожидает сообщения");
    expect(
      line({ state: "working", evidence: { phase: "generating" } }),
    ).toMatchObject({ text: "Генерирует", tone: "accent", live: false });
    expect(
      line({
        state: "working",
        evidence: { phase: "generating", summary: "retry 3" },
      }).text,
    ).toBe("Повторяет запрос к модели, попытка 3");
    expect(
      line({
        state: "working",
        evidence: { phase: "generating", summary: "Субагент general: done" },
      }).text,
    ).toBe("Генерирует");
    expect(
      line({ state: "failed", evidence: { reason: "session_stuck" } }).text,
    ).toBe("Сбой: один и тот же отказ провайдера подряд");
    expect(
      line({
        state: "working",
        evidence: {
          phase: "running_tool",
          tool_name: "cargo test",
          phase_since: new Date(now - 4 * 60_000).toISOString(),
        },
      }).text,
    ).toBe("Выполняет cargo test · 4 мин");
    // Отметка фазы из прошлого хода не тянет часы: ход начался 30 секунд назад.
    expect(
      line({
        state: "working",
        since: new Date(now - 30_000).toISOString(),
        evidence: {
          phase: "running_tool",
          tool_name: "cargo test",
          phase_since: new Date(now - 7 * 3_600_000).toISOString(),
        },
      }).text,
    ).toBe("Выполняет cargo test · 30 с");
    expect(
      line({ state: "working", evidence: { phase: "starting" } }).text,
    ).toBe("Запускается");
    expect(
      line({ state: "working", evidence: { phase: "stopping" } }).text,
    ).toBe("Останавливается");
    expect(
      line({ state: "working", evidence: { phase: "context_maintenance" } })
        .text,
    ).toBe("Сжимает контекст");
    expect(
      line({ state: "working", evidence: { phase: "delivering_attachments" } })
        .text,
    ).toBe("Передаю файлы на машину");
    expect(
      line({ state: "working", evidence: { phase: "no_link" } }),
    ).toMatchObject({ text: "Нет связи с машиной", tone: "warning" });
    expect(line({ state: "working", evidence: { phase: "other" } })).toEqual({
      text: "Работает",
      tone: "accent",
      live: true,
    });
    expect(
      line({
        state: "waiting_for_user",
        evidence: {
          question:
            "2 варианта и ещё очень длинный вопрос который надо обрезать прямо здесь на границе",
        },
      }).text.startsWith("Ждёт ответа · "),
    ).toBe(true);
    expect(
      line({
        state: "waiting_for_user",
        evidence: { question: "x".repeat(60) },
      }).text.length,
    ).toBe("Ждёт ответа · ".length + 48);
    expect(
      line({
        state: "stalled",
        evidence: { last_event_at: new Date(now - 12 * 60_000).toISOString() },
      }),
    ).toMatchObject({
      text: "Подвисла? Нет событий 12 мин",
      tone: "warning",
    });
    expect(
      line({
        state: "quota_paused",
        evidence: {
          reason: "quota_exhausted",
          until: new Date(now).toISOString(),
        },
      }).text,
    ).toContain("Пауза: лимит на 5 часов до");
    expect(
      line({ state: "quota_paused", evidence: { reason: "weekly_reserve" } })
        .text,
    ).toBe("Ожидание: недельный лимит");
    expect(
      line({
        state: "quota_paused",
        evidence: {
          reason: "limit_reached",
          provider: "codex",
          until: new Date(now).toISOString(),
        },
      }).text,
    ).toContain("Остановлена: лимит Codex до");
    expect(
      line({
        state: "quota_paused",
        evidence: { reason: "limit_reached", provider: "opencode" },
      }).text,
    ).toBe("Остановлена: лимит OpenCode");
    expect(
      line({ state: "failed", evidence: { reason: "owner_lost" } }),
    ).toEqual({
      text: "Сбой: потерян владелец исполнения",
      tone: "danger",
      live: false,
    });
    expect(
      line({ state: "failed", evidence: { reason: "context_exhausted" } }).text,
    ).toContain("исчерпан контекст");
    expect(
      line({ state: "failed", evidence: { reason: "crashed" } }).text,
    ).toContain("процесс завершился с ошибкой");
    expect(
      line({ state: "failed", evidence: { reason: "start_failed" } }).text,
    ).toContain("не удалось запустить");
    expect(
      line({ state: "failed", evidence: { reason: "protocol_incompatible" } })
        .text,
    ).toContain("несовместимый протокол агента");
    expect(
      line({ state: "failed", evidence: { reason: "start_rejected" } }).text,
    ).toContain("запуск отклонён машиной");
    expect(
      line({ state: "failed", evidence: { reason: "cli_incompatible" } }).text,
    ).toContain("несовместимая версия CLI");
    expect(
      line({ state: "failed", evidence: { reason: "auth_required" } }).text,
    ).toContain("агенту нужна авторизация");
    expect(
      line({ state: "failed", evidence: { reason: "storage_failure" } }).text,
    ).toContain("ошибка хранилища на машине");
    expect(
      line({ state: "failed", evidence: { reason: "supervisor_died" } }).text,
    ).toContain("процесс сопровождения завершился");
    expect(
      line({ state: "failed", evidence: { reason: "admin_resolved" } }).text,
    ).toContain("закрыто администратором");
    expect(
      line({ state: "failed", evidence: { reason: "mystery" } }).text,
    ).toBe("Сбой: причина неизвестна");
    expect(line({ state: "stopped" }).text).toBe("Завершена");
    expect(
      line({ state: "stopped", evidence: { outcome: "user_stop" } }).text,
    ).toBe("Остановлена вами");
    expect(
      line({ state: "stopped", evidence: { outcome: "force_stopped" } }).text,
    ).toBe("Остановлена принудительно");
  });

  it("activity_durations_use_the_server_corrected_clock", () => {
    const corrected = now + 60_000;
    const text = activityLine(
      {
        state: "stalled",
        evidence: { last_event_at: new Date(now).toISOString() },
      },
      { now: corrected },
    ).text;
    expect(text).toBe("Подвисла? Нет событий 1 мин");
    expect(text).not.toBe(
      activityLine(
        {
          state: "stalled",
          evidence: { last_event_at: new Date(now).toISOString() },
        },
        { now },
      ).text,
    );
  });
});

describe("background work", () => {
  const idle = (count: number | null): Activity => ({
    state: "idle",
    evidence: { kind: "idle", last_turn_at: "2026-11-01T00:00:00.000Z", background_work: count },
  });

  it("idle_with_background_work_is_a_live_accent_line", () => {
    expect(line(idle(2))).toEqual({
      text: "Ожидает сообщения · 2 фоновые",
      tone: "accent",
      live: true,
    });
    expect(line(idle(1)).text).toBe("Ожидает сообщения · 1 фоновая");
    expect(line(idle(5)).text).toBe("Ожидает сообщения · 5 фоновых");
  });

  it("idle_with_nothing_running_stays_quiet", () => {
    for (const count of [0, null]) {
      expect(line(idle(count))).toEqual({ text: "Ожидает сообщения", tone: "default", live: false });
      expect(chatStateLine(idle(count), null, "running", 0).quiet).toBe(true);
    }
  });

  it("header_shows_background_work_and_the_session_still_accepts_messages", () => {
    expect(chatStateLine(idle(2), null, "running", 0).quiet).toBe(false);
    expect(turnRunning(idle(2))).toBe(false);
  });
});
