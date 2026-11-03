import type { InputView } from "#lib/stores/inputs.js";
import type { OpJournal } from "#lib/ops/journal.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import type { DraftStore } from "#lib/drafts/store.js";
import { cancelInput as apiCancelInput, editInput as apiEditInput, sendNow as apiSendNow } from "#lib/api/client.js";
import { errorText, REASON_TEXT, type ErrorReason } from "#lib/api/errors.js";
import { ApiError, NetworkError } from "#lib/api/http.js";
import { intents } from "#lib/ops/intent.js";
import { returnToComposer } from "#lib/composer/returned.js";
import { freshInput, placeInput } from "#lib/stores/inputs.js";

export interface CancelDeps {
  clientCancel?: (
    id: string,
    key: string,
  ) => Promise<{ data?: unknown } | unknown>;
}

/**
 * Отмена сообщения в очереди или на машине.
 * Результаты соответствуют Delivery D3:
 * - 'cancelled': отмена подтверждена сервером
 * - 'requested': команда отмены отправлена исполнителю
 * - 'lost': хаб ответил, что передача уже началась, обычная отмена невозможна
 * - 'failed': отмена не дошла или отклонена; прерывать ход по ней нельзя
 */
export async function cancelInput(
  input: InputView,
  deps?: CancelDeps,
): Promise<"cancelled" | "requested" | "lost" | { failed: string }> {
  if (!input.id) {
    input.state = "cancelled";
    input.placement = "none";
    return "cancelled";
  }
  const id = input.id;

  try {
    const response = await intents.run(`cancel:${id}`, (key) =>
      deps?.clientCancel
        ? deps.clientCancel(id, key)
        : apiCancelInput(id, key),
    );

    const data = (response as { data?: unknown })?.data ?? response;

    if (data === "cancelled") {
      input.state = "cancelled";
      input.placement = "none";
      return "cancelled";
    }

    if (typeof data === "object" && data !== null) {
      if ("cancel_requested" in data) {
        input.cancelRequested = true;
        return "requested";
      }
      if ("too_late" in data) {
        return "lost";
      }
    }

    return { failed: "Хаб ответил непонятно, отмена не подтверждена" };
  } catch (err) {
    return {
      failed:
        err instanceof NetworkError
          ? "Нет связи, отмена не отправлена"
          : errorText(err, "Отменить не удалось"),
    };
  }
}

export interface RetryDeps {
  ops: OpJournal;
  chat: ChatStore;
  conversationId: string;
  confirm?: (message: string) => boolean | Promise<boolean>;
}

/**
 * Повторная отправка неподтверждённого сообщения как нового.
 * Создаёт новый ключ идемпотентности и передаёт retry_of, указывающий на предыдущий ввод (A4 G10).
 */
export async function retryAsNew(
  input: InputView,
  deps?: RetryDeps,
): Promise<void> {
  const warning =
    "Действие может быть выполнено повторно. Отправить как новое сообщение?";
  const confirmed = deps?.confirm
    ? await deps.confirm(warning)
    : typeof window !== "undefined" && typeof window.confirm === "function"
      ? window.confirm(warning)
      : true;

  if (!confirmed) return;
  if (!deps) return;

  const newKey = crypto.randomUUID();
  const retryTarget = input.id ?? input.key;

  const body = {
    content: [{ type: "text" as const, text: input.text }],
    attachment_ids: input.attachments.map((a) => a.attachment_id),
    retry_of: retryTarget,
  };

  const localInput = freshInput(
    newKey,
    input.text,
    Date.now(),
    input.attachments,
  );
  localInput.retryOf = retryTarget;
  deps.chat.inputs.set(newKey, localInput);

  const op = await deps.ops.begin("input", body, {
    conversationId: deps.conversationId,
    persist: true,
    key: newKey,
  });

  void deps.ops.run(op).catch((err: unknown) => {
    // Отказ хаба показываем на самом пузыре, а не теряем в необработанном промисе.
    const current = deps.chat.inputs.get(newKey);
    if (!current) return;
    deps.chat.inputs.set(newKey, {
      ...current,
      state: "rejected",
      placement: placeInput("rejected"),
      rejection: errorText(err, "Повторная отправка не удалась"),
    });
  });
}

export interface ReturnDraftDeps {
  drafts: DraftStore;
  conversationId: string;
  chat: ChatStore;
}

/**
 * Возврат отклонённого сообщения к автору: перед набранным текстом и вместе с файлами.
 */
export async function returnToDraft(
  input: InputView,
  deps: ReturnDraftDeps,
): Promise<void> {
  deps.chat.inputs.delete(input.key);
  deps.chat.timeline.remove(`input:${input.key}`);
  await returnToComposer(
    deps.conversationId,
    {
      text: input.text,
      attachments: input.attachments.map((item) => ({
        id: item.attachment_id,
        name: item.name,
        size: item.size,
      })),
    },
    deps.drafts,
  );
}

export interface SendNowDeps {
  clientSendNow?: (
    conversationId: string,
    inputId: string,
    key: string,
  ) => Promise<unknown>;
}

export type SendNowResult = "requested" | "noop" | { reason: string };

const SEND_NOW_REASONS = new Set([
  "input_not_queued",
  "no_running_turn",
  "request_open",
  "compacting",
  "unsupported_harness",
  "capability_missing",
]);

/**
 * «Отправить сейчас»: влить ожидающее сообщение в идущий ход. По умолчанию
 * сообщение ждёт конца хода; это действие — явный обход. Ход между кликом и
 * доставкой мог кончиться: тогда сообщение просто уйдёт следующим ходом.
 */
export async function sendNow(
  input: InputView,
  conversationId: string,
  deps?: SendNowDeps,
): Promise<SendNowResult> {
  if (!input.id) return "noop";
  const id = input.id;
  try {
    await intents.run(`send-now:${id}`, (key) =>
      deps?.clientSendNow
        ? deps.clientSendNow(conversationId, id, key)
        : apiSendNow(conversationId, id, key),
    );
    return "requested";
  } catch (err) {
    if (err instanceof NetworkError) return { reason: "network" };
    const reason = refusalReason(err);
    return { reason: SEND_NOW_REASONS.has(reason) ? reason : "unknown" };
  }
}

/** Вливание не удалось (агент отказал или ход кончился): сообщение ждёт следующего хода. */
export const STEER_RETURNED_LABEL = "Не удалось влить в ход: сообщение уйдёт следующим";

/** Причина отказа хаба независимо от кода: конфликты состояния идут 409, ошибки запроса 400. */
function refusalReason(err: unknown): string {
  return err instanceof ApiError && typeof err.details?.reason === "string"
    ? err.details.reason
    : "unknown";
}

/** Подпись отказа «Отправить сейчас» и скрытие кнопки, когда она неуместна. */
export function sendNowCaption(reason: string): string | null {
  switch (reason) {
    case "no_running_turn":
      return "Ход уже кончился: сообщение уйдёт следующим";
    case "request_open":
      return "Дождитесь ответа на запрос";
    case "compacting":
      return "Дождитесь конца сжатия";
    case "input_not_queued":
    case "unsupported_harness":
    case "capability_missing":
      return null;
    case "network":
      return "Нет связи с хабом, повторите";
    default:
      return "Не получилось отправить сейчас";
  }
}

export interface EditInputDeps {
  clientEditInput?: (
    conversationId: string,
    inputId: string,
    body: { text: string; attachment_ids: string[]; expected_revision: number },
    key: string,
  ) => Promise<unknown>;
}

export type EditInputResult = "saved" | "noop" | { reason: string };

const EDIT_INPUT_REASONS = new Set([
  "input_not_queued",
  "revision_conflict",
  "steer_in_flight",
  "empty_content",
  "content_too_long",
  "attachment_not_ready",
  "attachment_not_found",
  "attachment_unavailable",
  "capability_missing",
  "limits_mismatch",
]);

/**
 * Правка ожидающего ввода сравнением-и-заменой по ревизии. Устаревшая ревизия
 * и ушедший из очереди ввод отклоняются хабом с причиной, а не затирают чужое.
 */
export async function editInput(
  input: InputView,
  conversationId: string,
  text: string,
  attachmentIds: string[],
  expectedRevision: number,
  deps?: EditInputDeps,
): Promise<EditInputResult> {
  if (!input.id) return "noop";
  const id = input.id;
  const body = { text, attachment_ids: attachmentIds, expected_revision: expectedRevision };
  // Повтор той же правки после обрыва идёт тем же ключом, и хаб отвечает первым исходом.
  const scope = `edit:${id}:${JSON.stringify(body)}`;
  try {
    await intents.run(scope, (key) =>
      deps?.clientEditInput
        ? deps.clientEditInput(conversationId, id, body, key)
        : apiEditInput(conversationId, id, body, key),
    );
    return "saved";
  } catch (err) {
    if (err instanceof NetworkError) return { reason: "network" };
    const reason = refusalReason(err);
    // Своя же правка, дошедшая до хаба раньше потерянного ответа, не конфликт.
    if (
      reason === "revision_conflict" &&
      input.revision === expectedRevision + 1 &&
      input.text === text
    )
      return "saved";
    return { reason: EDIT_INPUT_REASONS.has(reason) ? reason : "unknown" };
  }
}

/** Подпись отказа правки; null прячет кнопку вместо подписи. */
export function editInputCaption(reason: string): string | null {
  switch (reason) {
    case "revision_conflict":
      return "Сообщение уже изменено в другом месте";
    case "input_not_queued":
      return "Сообщение уже ушло, править поздно";
    case "steer_in_flight":
      return "Сообщение вливается в ход, дождитесь конца хода";
    case "empty_content":
      return "Сообщение не должно быть пустым";
    case "content_too_long":
      return "Сообщение длиннее 256 КиБ";
    case "attachment_not_ready":
      return "Вложение ещё загружается";
    case "attachment_not_found":
    case "attachment_unavailable":
      return "Вложение недоступно";
    case "capability_missing":
    case "limits_mismatch":
      return "Машина сейчас не принимает вложения";
    case "network":
      return "Нет связи с хабом, правка не сохранена";
    default:
      return reason in REASON_TEXT
        ? REASON_TEXT[reason as ErrorReason]
        : "Не получилось сохранить";
  }
}
