import { describe, expect, it } from "vitest";
import {
  activityLine,
  type Activity,
} from "#lib/screens/sessions/activityText.js";

const now = Date.parse("2026-10-29T12:00:00.000Z");

function line(activity: Activity) {
  return activityLine(activity, { now });
}

describe("sessions/activity", () => {
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
        evidence: {
          phase: "running_tool",
          tool_name: "cargo test",
          phase_since: new Date(now - 4 * 60_000).toISOString(),
        },
      }).text,
    ).toBe("Выполняет cargo test · 4 мин");
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
