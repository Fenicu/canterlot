import type { InputView } from "#lib/stores/inputs.js";
import type { OpJournal } from "#lib/ops/journal.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import type { DraftStore } from "#lib/drafts/store.js";
import { cancelInput as apiCancelInput, sendNow as apiSendNow } from "#lib/api/client.js";
import { ApiError } from "#lib/api/http.js";
import { returnToComposer } from "#lib/composer/returned.js";
import { freshInput } from "#lib/stores/inputs.js";

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
 * - 'lost': передача уже началась, обычная отмена невозможна
 */
export async function cancelInput(
  input: InputView,
  deps?: CancelDeps,
): Promise<"cancelled" | "requested" | "lost"> {
  if (!input.id) {
    input.state = "cancelled";
    input.placement = "none";
    return "cancelled";
  }

  try {
    const key = crypto.randomUUID();
    const response = deps?.clientCancel
      ? await deps.clientCancel(input.id, key)
      : await apiCancelInput(input.id, key);

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

    return "lost";
  } catch {
    return "lost";
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

  void deps.ops.run(op);
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
  try {
    const key = crypto.randomUUID();
    if (deps?.clientSendNow) await deps.clientSendNow(conversationId, input.id, key);
    else await apiSendNow(conversationId, input.id, key);
    return "requested";
  } catch (err) {
    if (err instanceof ApiError) {
      const reason =
        err.code === "invalid_request" && typeof err.details?.reason === "string"
          ? err.details.reason
          : "unknown";
      if (reason === "unknown" || SEND_NOW_REASONS.has(reason)) return { reason };
    }
    return { reason: "unknown" };
  }
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
    default:
      return "Не получилось отправить сейчас";
  }
}
