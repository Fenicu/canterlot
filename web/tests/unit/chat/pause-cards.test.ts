import { describe, expect, it } from "vitest";
import { fmt } from "#lib/format/ru.js";
import {
  pauseHoldCards,
  runLabel,
  type Card,
  type CardAction,
} from "#lib/screens/chat/pauseCards.js";
import type { components } from "#lib/api/generated/openapi.js";

type PauseReason = components["schemas"]["PauseReason"];
type HoldView = components["schemas"]["HoldView"];
type QueueView = components["schemas"]["QueueView"];

const NOW = Date.parse("2026-10-29T12:00:00.000Z");
const RESET_AT_ISO = "2026-10-30T09:00:00.000Z";
const RESET_AT_FMT = fmt.resetAt(Date.parse(RESET_AT_ISO), NOW);
const WAKE_AT_ISO = "2026-10-29T13:30:00.000Z";
const WAKE_AT_FMT = fmt.clock(Date.parse(WAKE_AT_ISO));

describe("chat/pauseCards", () => {
  it("pause_hold_cards_cover_every_reason_with_the_exact_action", () => {
    // Table over every PauseReason
    const pauseCases: {
      reason: PauseReason;
      liveExecution?: boolean;
      expectedTitle: string;
      expectedTone: Card["tone"];
      expectedActionIds: CardAction["id"][];
    }[] = [
      {
        reason: { reason: "user_stop" },
        liveExecution: true,
        expectedTitle: "Очередь остановлена: вы прервали работу",
        expectedTone: "warning",
        expectedActionIds: ["continue_queue"],
      },
      {
        reason: { reason: "user_stop" },
        liveExecution: false,
        expectedTitle: "Очередь остановлена: вы прервали работу",
        expectedTone: "warning",
        expectedActionIds: ["run"],
      },
      {
        reason: { reason: "fatal_error" },
        expectedTitle: "Очередь остановлена из-за ошибки агента",
        expectedTone: "danger",
        expectedActionIds: ["run"],
      },
      {
        reason: { reason: "fatal_error" },
        liveExecution: true,
        expectedTitle: "Очередь остановлена из-за ошибки агента",
        expectedTone: "danger",
        expectedActionIds: ["continue_queue"],
      },
      {
        reason: { reason: "process_exited" },
        liveExecution: false,
        expectedTitle: "Процесс агента завершился",
        expectedTone: "warning",
        expectedActionIds: ["run"],
      },
      {
        reason: { reason: "process_exited" },
        liveExecution: true,
        expectedTitle: "Процесс агента завершился",
        expectedTone: "warning",
        expectedActionIds: ["continue_queue"],
      },
      {
        reason: {
          reason: "limit_reached",
          provider: "claude",
          resets_at: RESET_AT_ISO,
          window: "5h",
        },
        expectedTitle: `Остановлена: достигнут лимит Claude, сброс ${RESET_AT_FMT}; без «кранча» продолжение не отправляется`,
        expectedTone: "warning",
        expectedActionIds: ["run"],
      },
      {
        reason: {
          reason: "unconfirmed_delivery",
          input_id: "00000000-0000-4000-8000-000000000001",
        },
        expectedTitle: "Приём сообщения не подтверждён",
        expectedTone: "warning",
        expectedActionIds: ["retry_as_new", "run"],
      },
      {
        reason: { reason: "owner_lost" },
        expectedTitle: "Связь с исполнением потеряна",
        expectedTone: "danger",
        expectedActionIds: ["run"],
      },
      {
        reason: { reason: "restore_pending" },
        expectedTitle: "Восстановление сессии",
        expectedTone: "warning",
        expectedActionIds: [],
      },
    ];

    for (const tc of pauseCases) {
      const q: QueueView = {
        pause_reasons: [tc.reason],
        hold_reasons: [],
        queued: 1,
      };
      const cards = pauseHoldCards(q, {
        provider: "claude",
        crunch: false,
        resumeText: null,
        liveExecution: tc.liveExecution ?? false,
        now: NOW,
      });
      expect(cards.length, `Expected card for pause ${tc.reason.reason}`).toBe(
        1,
      );
      expect(cards[0]!.title).toBe(tc.expectedTitle);
      expect(cards[0]!.tone).toBe(tc.expectedTone);
      expect(cards[0]!.actions.map((a) => a.id)).toEqual(tc.expectedActionIds);
    }

    // Table over every HoldView
    const holdCases: {
      hold: HoldView;
      queued?: number;
      expectedOmitted?: boolean;
      expectedTitle?: string;
      expectedTone?: Card["tone"];
      expectedActionIds?: CardAction["id"][];
      expectedBody?: string[];
    }[] = [
      {
        hold: { reason: "run_in_progress" },
        expectedOmitted: true,
      },
      {
        hold: { reason: "awaiting_user_request" },
        expectedOmitted: true,
      },
      {
        hold: { reason: "no_live_execution" },
        queued: 1,
        expectedTitle: "Сессия не запущена",
        expectedTone: "default",
        expectedActionIds: ["run"],
      },
      {
        hold: { reason: "no_live_execution" },
        queued: 0,
        expectedTitle: "Сессия не запущена",
        expectedTone: "default",
        expectedActionIds: ["run"],
      },
      {
        hold: { reason: "execution_starting" },
        expectedTitle: "Сессия запускается",
        expectedTone: "default",
        expectedActionIds: [],
      },
      {
        hold: { reason: "context_maintenance" },
        expectedTitle: "Сжатие контекста",
        expectedTone: "default",
        expectedActionIds: [],
      },
      {
        hold: { reason: "storage_low" },
        expectedTitle: "Недостаточно места на диске",
        expectedTone: "danger",
        expectedActionIds: [],
      },
      {
        hold: {
          reason: "workdir_busy",
          conflict_conversation_id: "00000000-0000-4000-8000-000000000002",
        },
        expectedTitle: "Рабочая директория занята другой сессией",
        expectedTone: "warning",
        expectedActionIds: [],
      },
      {
        hold: {
          reason: "attachments_pending",
          input_id: "00000000-0000-4000-8000-000000000003",
        },
        expectedTitle: "Ждём передачи файлов на машину",
        expectedTone: "default",
        expectedActionIds: [],
        expectedBody: [],
      },
      {
        hold: {
          reason: "attachments_pending",
          input_id: "00000000-0000-4000-8000-000000000003",
          machine_reason: "hub_unreachable",
        },
        expectedTitle: "Ждём передачи файлов на машину",
        expectedTone: "default",
        expectedActionIds: [],
        expectedBody: [
          "Хаб недоступен, передача продолжится после восстановления связи",
        ],
      },
      {
        hold: {
          reason: "attachments_pending",
          input_id: "00000000-0000-4000-8000-000000000003",
          machine_reason: "transfer_retrying",
        },
        expectedTitle: "Ждём передачи файлов на машину",
        expectedTone: "default",
        expectedActionIds: [],
        expectedBody: ["Передача повторяется"],
      },
      {
        hold: {
          reason: "quota_exhausted",
          provider: "claude",
          wake_at: WAKE_AT_ISO,
          window: "5h",
        },
        expectedTitle: `Пауза до ${WAKE_AT_FMT}, затем продолжу автоматически`,
        expectedTone: "warning",
        expectedActionIds: [],
        expectedBody: ["Вкладку можно закрыть: продолжение отправит хаб."],
      },
      {
        hold: {
          reason: "weekly_reserve",
          provider: "claude",
          resets_at: RESET_AT_ISO,
        },
        expectedTitle: "Пауза: недельный лимит Claude 95% · резерв владельца",
        expectedTone: "warning",
        expectedActionIds: ["spend_reserve", "edit_resume_text"],
      },
      {
        hold: { reason: "machine_not_reconciled" },
        expectedTitle: "Машина сверяет состояние после перезапуска",
        expectedTone: "warning",
        expectedActionIds: [],
      },
      {
        hold: {
          reason: "capability_missing",
          input_id: "00000000-0000-4000-8000-000000000004",
        },
        expectedTitle:
          "Машине нужна версия воркера с поддержкой пилота: сообщение ждёт",
        expectedTone: "warning",
        expectedActionIds: [],
      },
    ];

    for (const tc of holdCases) {
      const q: QueueView = {
        pause_reasons: [],
        hold_reasons: [tc.hold],
        queued: tc.queued ?? 0,
      };
      const cards = pauseHoldCards(q, {
        provider: "claude",
        crunch: false,
        resumeText: null,
        liveExecution: false,
        now: NOW,
      });
      if (tc.expectedOmitted) {
        expect(
          cards.length,
          `Expected hold ${tc.hold.reason} to be omitted`,
        ).toBe(0);
      } else {
        expect(cards.length, `Expected card for hold ${tc.hold.reason}`).toBe(
          1,
        );
        expect(cards[0]!.title).toBe(tc.expectedTitle);
        expect(cards[0]!.tone).toBe(tc.expectedTone);
        expect(cards[0]!.actions.map((a) => a.id)).toEqual(
          tc.expectedActionIds,
        );
        if (tc.expectedBody) {
          expect(cards[0]!.body).toEqual(tc.expectedBody);
        }
      }
    }
  });

  it("weekly_reserve_card_shows_reset_time_and_override_only_in_that_state", () => {
    const weeklyQueue: QueueView = {
      pause_reasons: [],
      hold_reasons: [
        {
          reason: "weekly_reserve",
          provider: "claude",
          resets_at: RESET_AT_ISO,
        },
      ],
      queued: 0,
    };
    const cards = pauseHoldCards(weeklyQueue, {
      provider: "claude",
      crunch: true,
      resumePending: true,
      resumeText: null,
      liveExecution: false,
      now: NOW,
    });
    expect(cards.length).toBe(1);
    expect(cards[0]!.title).toContain(
      "Пауза: недельный лимит Claude 95% · резерв владельца",
    );
    expect(cards[0]!.body.some((b) => b.includes(RESET_AT_FMT))).toBe(true);
    expect(cards[0]!.actions.some((a) => a.id === "spend_reserve")).toBe(true);

    const otherQueue: QueueView = {
      pause_reasons: [{ reason: "user_stop" }],
      hold_reasons: [],
      queued: 1,
    };
    const otherCards = pauseHoldCards(otherQueue, {
      provider: "claude",
      crunch: true,
      resumePending: true,
      resumeText: null,
      liveExecution: true,
      now: NOW,
    });
    expect(
      otherCards.some((c) => c.actions.some((a) => a.id === "spend_reserve")),
    ).toBe(false);
  });

  it("weekly_card_promises_automatic_continuation_only_for_resume_pending_crunch_sessions", () => {
    const weeklyQueue: QueueView = {
      pause_reasons: [],
      hold_reasons: [
        {
          reason: "weekly_reserve",
          provider: "claude",
          resets_at: RESET_AT_ISO,
        },
      ],
      queued: 0,
    };

    // Crunch enabled AND resume pending
    const autoCards = pauseHoldCards(weeklyQueue, {
      provider: "claude",
      crunch: true,
      resumePending: true,
      resumeText: "Продолжай",
      liveExecution: false,
      now: NOW,
    });
    expect(
      autoCards[0]!.body.some((b) => b.includes("Автоматическое продолжение")),
    ).toBe(true);
    expect(
      autoCards[0]!.body.some((b) => b.includes("Пауза снимется после сброса")),
    ).toBe(false);

    // Crunch disabled
    const noCrunchCards = pauseHoldCards(weeklyQueue, {
      provider: "claude",
      crunch: false,
      resumePending: false,
      resumeText: null,
      liveExecution: false,
      now: NOW,
    });
    expect(
      noCrunchCards[0]!.body.some((b) =>
        b.includes("Пауза снимется после сброса недельного лимита"),
      ),
    ).toBe(true);
    expect(
      noCrunchCards[0]!.body.some((b) =>
        b.includes("Автоматическое продолжение"),
      ),
    ).toBe(false);

    // Crunch enabled but NOT resume_pending
    const notPendingCards = pauseHoldCards(weeklyQueue, {
      provider: "claude",
      crunch: true,
      resumePending: false,
      resumeText: null,
      liveExecution: false,
      now: NOW,
    });
    expect(
      notPendingCards[0]!.body.some((b) =>
        b.includes("Пауза снимется после сброса недельного лимита"),
      ),
    ).toBe(true);
    expect(
      notPendingCards[0]!.body.some((b) =>
        b.includes("Автоматическое продолжение"),
      ),
    ).toBe(false);
  });

  it("five_hour_card_says_the_tab_may_be_closed", () => {
    const q: QueueView = {
      pause_reasons: [],
      hold_reasons: [
        {
          reason: "quota_exhausted",
          provider: "claude",
          wake_at: WAKE_AT_ISO,
          window: "5h",
        },
      ],
      queued: 0,
    };
    const cards = pauseHoldCards(q, {
      provider: "claude",
      crunch: false,
      resumeText: null,
      liveExecution: false,
      now: NOW,
    });
    expect(cards.length).toBe(1);
    expect(cards[0]!.title).toBe(
      `Пауза до ${WAKE_AT_FMT}, затем продолжу автоматически`,
    );
    expect(cards[0]!.body).toContain(
      "Вкладку можно закрыть: продолжение отправит хаб.",
    );
  });

  it("limit_pause_without_crunch_states_the_reason_and_reset_without_auto_continue", () => {
    const q: QueueView = {
      pause_reasons: [
        {
          reason: "limit_reached",
          provider: "claude",
          resets_at: RESET_AT_ISO,
          window: "5h",
        },
      ],
      hold_reasons: [],
      queued: 1,
    };
    const cards = pauseHoldCards(q, {
      provider: "claude",
      crunch: false,
      resumeText: null,
      liveExecution: false,
      now: NOW,
    });
    expect(cards.length).toBe(1);
    expect(cards[0]!.title).toBe(
      `Остановлена: достигнут лимит Claude, сброс ${RESET_AT_FMT}; без «кранча» продолжение не отправляется`,
    );
    expect(
      cards[0]!.body.some((b) => b.includes("Автоматическое продолжение")),
    ).toBe(false);
  });

  it("pause_with_no_live_execution_offers_resume_and_continue_not_continue_alone", () => {
    const q: QueueView = {
      pause_reasons: [{ reason: "user_stop" }],
      hold_reasons: [{ reason: "no_live_execution" }],
      queued: 1,
    };
    const cards = pauseHoldCards(q, {
      provider: "claude",
      crunch: false,
      resumeText: null,
      liveExecution: false,
      now: NOW,
    });
    expect(cards.length).toBeGreaterThan(0);
    // Карточка остановки без живого исполнения предлагает единое действие
    const stopCard = cards.find((c) => c.key === "pause:user_stop");
    expect(stopCard).toBeDefined();
    expect(stopCard!.actions.map((a) => a.id)).toEqual(["run"]);
    expect(stopCard!.actions.some((a) => a.id === "continue_queue")).toBe(
      false,
    );

    // With live execution, user_stop offers continue_queue
    const liveCards = pauseHoldCards(
      {
        pause_reasons: [{ reason: "user_stop" }],
        hold_reasons: [],
        queued: 1,
      },
      {
        provider: "claude",
        crunch: false,
        resumeText: null,
        liveExecution: true,
        now: NOW,
      },
    );
    const liveStopCard = liveCards.find((c) => c.key === "pause:user_stop");
    expect(liveStopCard).toBeDefined();
    expect(liveStopCard!.actions.map((a) => a.id)).toEqual(["continue_queue"]);
  });

  const base = {
    provider: "claude",
    crunch: false,
    resumeText: null,
    liveExecution: false,
    now: NOW,
  };
  const failedStart: QueueView = {
    pause_reasons: [{ reason: "fatal_error" }],
    hold_reasons: [{ reason: "no_live_execution" }],
    queued: 0,
  };

  it("the_single_action_is_labelled_by_whether_a_turn_was_recorded", () => {
    expect(runLabel(false)).toBe("Запустить");
    expect(runLabel(true)).toBe("Возобновить");
    const fresh = pauseHoldCards(
      { pause_reasons: [], hold_reasons: [{ reason: "no_live_execution" }], queued: 0 },
      { ...base, turnRecorded: false },
    );
    expect(fresh[0]!.actions).toEqual([{ id: "run", label: "Запустить" }]);
    const used = pauseHoldCards(
      { pause_reasons: [], hold_reasons: [{ reason: "no_live_execution" }], queued: 2 },
      { ...base, turnRecorded: true },
    );
    expect(used[0]!.actions).toEqual([{ id: "run", label: "Возобновить" }]);
  });

  it("a_failed_start_is_not_a_dead_end_and_names_the_reason", () => {
    const cards = pauseHoldCards(failedStart, {
      ...base,
      turnRecorded: false,
      failureReason: "start_rejected",
    });
    expect(cards.map((card) => card.key)).toEqual(["pause:fatal_error"]);
    expect(cards[0]!.actions).toEqual([{ id: "run", label: "Запустить" }]);
    expect(cards[0]!.body).toEqual(["Причина: запуск отклонён машиной"]);
  });

  it("an_unknown_failure_reason_adds_no_line", () => {
    const cards = pauseHoldCards(failedStart, { ...base, failureReason: null });
    expect(cards[0]!.body).toEqual([]);
  });

  it("every_pause_but_restore_offers_a_way_forward", () => {
    const reasons: QueueView["pause_reasons"] = [
      { reason: "user_stop" },
      { reason: "fatal_error" },
      { reason: "process_exited" },
      { reason: "owner_lost" },
      { reason: "unconfirmed_delivery", input_id: "00000000-0000-4000-8000-000000000001" },
      { reason: "limit_reached", provider: "claude", resets_at: RESET_AT_ISO, window: "five_hour" },
    ];
    for (const liveExecution of [false, true]) {
      for (const reason of reasons) {
        const cards = pauseHoldCards(
          { pause_reasons: [reason], hold_reasons: [], queued: 1 },
          { ...base, liveExecution },
        );
        expect(cards[0]!.actions.length, `${reason.reason} live=${liveExecution}`).toBeGreaterThan(0);
      }
    }
  });

  it("an_execution_still_in_owner_lost_offers_nothing_the_hub_would_refuse", () => {
    const cards = pauseHoldCards(
      { pause_reasons: [{ reason: "owner_lost" }], hold_reasons: [], queued: 0 },
      { ...base, executionOwnerLost: true },
    );
    expect(cards[0]!.actions).toEqual([]);
    expect(cards[0]!.body).toEqual([
      "Продолжить можно, когда машина вернётся на связь и сверит состояние.",
    ]);
  });

  it("retry_as_new_names_the_input_it_repeats", () => {
    const inputId = "00000000-0000-4000-8000-000000000001";
    const cards = pauseHoldCards(
      { pause_reasons: [{ reason: "unconfirmed_delivery", input_id: inputId }], hold_reasons: [], queued: 0 },
      { ...base, liveExecution: true },
    );
    expect(cards[0]!.actions[0]).toMatchObject({ id: "retry_as_new", inputId });
    expect(cards[0]!.actions[1]?.id).toBe("continue_queue");
  });
});
