import { compactionLabel, failureLabel, providerLabel } from "#lib/format/labels.js";
import { fmt } from "#lib/format/ru.js";
import type { components } from "#lib/api/generated/openapi.js";

export type PauseReason = components["schemas"]["PauseReason"];
export type HoldView = components["schemas"]["HoldView"];
export type QueueView = components["schemas"]["QueueView"];

export interface Card {
  key: string;
  tone: "warning" | "default" | "danger";
  title: string;
  body: string[];
  actions: CardAction[];
}

export type CardAction = {
  id:
    | "continue_queue"
    | "run"
    | "spend_reserve"
    | "edit_resume_text"
    | "retry_as_new"
    | "compact_context"
    | "resolve_owner_lost";
  label: string;
  /** Ввод, который повторяет `retry_as_new`. */
  inputId?: string;
};

export interface PauseCardsContext {
  provider: string | null;
  crunch: boolean;
  resumePending?: boolean;
  resumeText: string | null;
  liveExecution: boolean;
  now: number;
  /** Записанный ход: единое действие продолжит беседу, а не начнёт новую. */
  turnRecorded?: boolean;
  /** Исполнение ещё в `owner_lost`: хаб отказал бы в запуске до сверки с машиной. */
  executionOwnerLost?: boolean;
  /** Причина сбоя из активности сессии. */
  failureReason?: string | null;
}

const ACTION_LABELS: Record<Exclude<CardAction["id"], "run">, string> = {
  continue_queue: "Продолжить очередь",
  spend_reserve: "Продолжить, тратя резерв",
  edit_resume_text: "Изменить текст продолжения",
  retry_as_new: "Отправить повторно как новое",
  compact_context: "Сжать контекст",
  resolve_owner_lost: "Завершить исполнение",
};

export const RESOLVE_WARNING =
  "Оставшиеся процессы исполнения получат сигнал завершения, незавершённая работа в них пропадёт. Если сопровождение на самом деле живо, сессия вернётся без потерь. Если подтвердить завершение не выйдет, исполнение всё равно закроется, а часть процессов может продолжить работу.";

/** Что осталось неизвестным после аварийного выхода: владелец видит это рядом с причиной. */
const EXIT_NOTES: Record<string, string> = {
  supervisor_died:
    "Последний вывод мог не сохраниться. При возобновлении контекст берётся из истории агента.",
  admin_resolved:
    "Часть процессов могла продолжить работу. Проверьте рабочую папку, прежде чем возобновлять.",
};

const STUCK_NOTE = "Причина отказа неизвестна. Сжатие контекста может его снять и тратит токены.";

/** Единое действие запуска: хаб сам решает, продолжить ли беседу. */
export function runLabel(turnRecorded: boolean | undefined): string {
  return turnRecorded ? "Возобновить" : "Запустить";
}

export function pauseHoldCards(q: QueueView, ctx: PauseCardsContext): Card[] {
  const cards: Card[] = [];
  const hasNoLiveExecutionHold = q.hold_reasons.some(
    (h) => h.reason === "no_live_execution",
  );
  const effectiveLive = ctx.liveExecution && !hasNoLiveExecutionHold;
  const hasPauses = q.pause_reasons.length > 0;
  const run: CardAction = { id: "run", label: runLabel(ctx.turnRecorded) };
  const forward: CardAction[] = ctx.executionOwnerLost
    ? []
    : [
        effectiveLive
          ? { id: "continue_queue", label: ACTION_LABELS.continue_queue }
          : run,
      ];

  // Process pause reasons
  for (const p of q.pause_reasons) {
    switch (p.reason) {
      case "user_stop": {
        cards.push({
          key: "pause:user_stop",
          tone: "warning",
          title: "Очередь остановлена: вы прервали работу",
          body: [],
          actions: forward,
        });
        break;
      }
      case "fatal_error": {
        cards.push({
          key: "pause:fatal_error",
          tone: "danger",
          title: "Очередь остановлена из-за ошибки агента",
          body: ctx.failureReason
            ? [
                `Причина: ${failureLabel(ctx.failureReason)}`,
                ...(EXIT_NOTES[ctx.failureReason]
                  ? [EXIT_NOTES[ctx.failureReason]!]
                  : []),
              ]
            : [],
          actions: forward,
        });
        break;
      }
      case "process_exited": {
        cards.push({
          key: "pause:process_exited",
          tone: "warning",
          title: "Процесс агента завершился",
          body: [],
          actions: forward,
        });
        break;
      }
      case "limit_reached": {
        const name = providerLabel(p.provider ?? ctx.provider ?? "claude");
        const resetTime = p.resets_at
          ? fmt.resetAt(Date.parse(p.resets_at), ctx.now)
          : "";
        const reset = resetTime ? `, сброс ${resetTime}` : "";
        const title = `Остановлена: достигнут лимит ${name}${reset}; без «кранча» продолжение не отправляется`;
        cards.push({
          key: "pause:limit_reached",
          tone: "warning",
          title,
          body: [],
          actions: forward,
        });
        break;
      }
      case "unconfirmed_delivery": {
        cards.push({
          key: `pause:unconfirmed_delivery:${p.input_id}`,
          tone: "warning",
          title: "Приём сообщения не подтверждён",
          body: [],
          actions: [
            {
              id: "retry_as_new",
              label: ACTION_LABELS.retry_as_new,
              inputId: p.input_id,
            },
            ...forward,
          ],
        });
        break;
      }
      case "owner_lost": {
        cards.push({
          key: "pause:owner_lost",
          tone: "danger",
          title: "Связь с исполнением потеряна",
          body: ctx.executionOwnerLost
            ? [
                "Если процессы исполнения завершились, сессия закроется сама. Если что-то ещё работает, машина ждёт вашего решения.",
              ]
            : [],
          actions: ctx.executionOwnerLost
            ? [
                {
                  id: "resolve_owner_lost",
                  label: ACTION_LABELS.resolve_owner_lost,
                },
              ]
            : forward,
        });
        break;
      }
      case "restore_pending": {
        cards.push({
          key: "pause:restore_pending",
          tone: "warning",
          title: "Восстановление сессии",
          body: [],
          actions: [],
        });
        break;
      }
      case "session_stuck": {
        const progress = compactionLabel(p.compaction);
        const requested = p.compaction === "requested";
        const compact: CardAction[] = effectiveLive
          ? [{ id: "compact_context", label: ACTION_LABELS.compact_context }]
          : [];
        cards.push({
          key: "pause:session_stuck",
          tone: "danger",
          title: `Очередь остановлена: ${failureLabel("session_stuck")}`,
          body: requested
            ? [progress ?? ""]
            : [...(progress ? [progress] : []), STUCK_NOTE],
          actions: requested ? [] : [...compact, ...forward],
        });
        break;
      }
    }
  }

  // Process hold reasons
  for (const h of q.hold_reasons) {
    switch (h.reason) {
      case "run_in_progress":
      case "awaiting_user_request":
        // Filtered out / handled elsewhere
        break;

      case "no_live_execution": {
        if (hasPauses) {
          // Пауза сама предлагает единое действие, а `restore_pending` ждёт процедуры восстановления.
          break;
        }
        cards.push({
          key: "hold:no_live_execution",
          tone: "default",
          title: "Сессия не запущена",
          body: [],
          actions: [run],
        });
        break;
      }

      case "execution_starting": {
        cards.push({
          key: "hold:execution_starting",
          tone: "default",
          title: "Сессия запускается",
          body: [],
          actions: [],
        });
        break;
      }

      case "context_maintenance": {
        cards.push({
          key: "hold:context_maintenance",
          tone: "default",
          title: "Сжатие контекста",
          body: [],
          actions: [],
        });
        break;
      }

      case "storage_low": {
        cards.push({
          key: "hold:storage_low",
          tone: "danger",
          title: "Недостаточно места на диске",
          body: [],
          actions: [],
        });
        break;
      }

      case "workdir_busy": {
        cards.push({
          key: `hold:workdir_busy:${h.conflict_conversation_id}`,
          tone: "warning",
          title: "Рабочая директория занята другой сессией",
          body: [],
          actions: [],
        });
        break;
      }

      case "attachments_pending": {
        const body: string[] = [];
        if (h.machine_reason === "hub_unreachable") {
          body.push(
            "Хаб недоступен, передача продолжится после восстановления связи",
          );
        } else if (h.machine_reason === "transfer_retrying") {
          body.push("Передача повторяется");
        }
        cards.push({
          key: `hold:attachments_pending:${h.input_id}`,
          tone: "default",
          title: "Ждём передачи файлов на машину",
          body,
          actions: [],
        });
        break;
      }

      case "quota_exhausted": {
        const time = fmt.clock(Date.parse(h.wake_at));
        cards.push({
          key: "hold:quota_exhausted",
          tone: "warning",
          title: `Пауза до ${time}, затем продолжу автоматически`,
          body: ["Вкладку можно закрыть: продолжение отправит хаб."],
          actions: [],
        });
        break;
      }

      case "weekly_reserve": {
        const name = providerLabel(h.provider ?? ctx.provider ?? "claude");
        const body: string[] = [];
        if (ctx.crunch && ctx.resumePending) {
          const resetStr = h.resets_at
            ? fmt.resetAt(Date.parse(h.resets_at), ctx.now)
            : "";
          body.push(`Автоматическое продолжение · ${resetStr}`);
        } else {
          body.push("Пауза снимется после сброса недельного лимита.");
        }
        body.push("Оставшийся резерв сохранён для ваших задач.");
        cards.push({
          key: "hold:weekly_reserve",
          tone: "warning",
          title: `Пауза: недельный лимит ${name} 95% · резерв владельца`,
          body,
          actions: [
            { id: "spend_reserve", label: ACTION_LABELS.spend_reserve },
            { id: "edit_resume_text", label: ACTION_LABELS.edit_resume_text },
          ],
        });
        break;
      }

      case "machine_not_reconciled": {
        cards.push({
          key: "hold:machine_not_reconciled",
          tone: "warning",
          title: "Машина сверяет состояние после перезапуска",
          body: [],
          actions: [],
        });
        break;
      }

      case "capability_missing": {
        cards.push({
          key: `hold:capability_missing:${h.input_id}`,
          tone: "warning",
          title:
            "Машине нужна версия воркера с поддержкой пилота: сообщение ждёт",
          body: [],
          actions: [],
        });
        break;
      }
    }
  }

  // Sort most severe first: danger (0) > warning (1) > default (2)
  const score: Record<Card["tone"], number> = {
    danger: 0,
    warning: 1,
    default: 2,
  };
  cards.sort((a, b) => score[a.tone] - score[b.tone]);

  return cards;
}
