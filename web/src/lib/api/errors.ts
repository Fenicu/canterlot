import { ApiError, AuthRequired, NetworkError } from "#lib/api/http.js";
import type { components } from "#lib/api/generated/openapi.js";

export type ErrorCode = components["schemas"]["ErrorCode"];

export type ErrorAction =
  | "retry_same_key"
  | "new_operation"
  | "restore_connection"
  | "check_acceptance"
  | "stop_process"
  | "sign_in_again"
  | "reload"
  | "none";

interface ErrorDescriptor {
  action: ErrorAction;
  message: string;
}

const ACTION_LABEL: Record<ErrorAction, string | null> = {
  retry_same_key: "Повторить",
  new_operation: "Отправить заново",
  restore_connection: "Проверить связь",
  check_acceptance: "Проверить принятие",
  stop_process: "Остановить процесс",
  sign_in_again: "Войти снова",
  reload: "Обновить страницу",
  none: null,
};

const ERROR_TABLE: Record<ErrorCode, ErrorDescriptor> = {
  unauthenticated: {
    action: "sign_in_again",
    message: "Требуется вход через сервис авторизации",
  },
  forbidden_user: {
    action: "sign_in_again",
    message: "Доступ для этого пользователя запрещён",
  },
  origin_rejected: {
    action: "sign_in_again",
    message: "Запрос отклонён: недопустимый источник",
  },
  invalid_request: {
    action: "none",
    message: "Некорректный запрос",
  },
  invalid_offset: {
    action: "none",
    message: "Недопустимое смещение",
  },
  not_found: {
    action: "none",
    message: "Не найдено",
  },
  method_not_allowed: {
    action: "none",
    message: "Метод не поддерживается",
  },
  idempotency_conflict: {
    action: "new_operation",
    message: "Ключ идемпотентности уже использован с другим запросом",
  },
  decision_conflict: {
    action: "none",
    message: "Решение уже принято с другого устройства",
  },
  request_not_open: {
    action: "none",
    message: "Запрос уже закрыт",
  },
  decision_invalid: {
    action: "none",
    message: "Ответ не подходит к вопросу",
  },
  resource_limit: {
    action: "none",
    message: "Превышен допустимый размер данных",
  },
  not_implemented: {
    action: "none",
    message: "Функция пока не реализована",
  },
  too_many_requests: {
    action: "retry_same_key",
    message: "Слишком много запросов, повторите позже",
  },
  db_unavailable: {
    action: "retry_same_key",
    message: "База данных временно недоступна",
  },
  internal: {
    action: "retry_same_key",
    message: "Внутренняя ошибка хаба",
  },
  queue_full: {
    action: "check_acceptance",
    message: "Очередь разговора заполнена",
  },
  conversation_closed: {
    action: "none",
    message: "Разговор закрыт",
  },
  execution_already_live: {
    action: "check_acceptance",
    message: "Исполнение уже запущено",
  },
  invalid_state: {
    action: "none",
    message: "Действие недопустимо в текущем состоянии",
  },
  not_pending: {
    action: "none",
    message: "Отложенное сообщение уже обработано",
  },
  schedule_time_out_of_range: {
    action: "none",
    message: "Время отправки вне допустимого диапазона",
  },
  too_many_scheduled: {
    action: "none",
    message: "Слишком много отложенных сообщений",
  },
  machine_not_found: {
    action: "none",
    message: "Машина не найдена",
  },
  machine_unavailable: {
    action: "restore_connection",
    message: "Машина недоступна",
  },
  capability_missing: {
    action: "restore_connection",
    message: "Требуется обновление воркера",
  },
  limits_mismatch: {
    action: "none",
    message: "Лимиты вложений машины не совпадают",
  },
  harness_unknown: {
    action: "none",
    message: "Харнесс отсутствует в каталоге машины",
  },
  model_unknown: {
    action: "none",
    message: "Модель отсутствует в каталоге харнесса",
  },
  effort_not_allowed: {
    action: "none",
    message: "Усилие недоступно для модели",
  },
  permission_mode_unknown: {
    action: "none",
    message: "Режим разрешений отсутствует в каталоге",
  },
  settings_midsession_unsupported: {
    action: "none",
    message: "Смена настроек во время исполнения не поддерживается",
  },
  profile_unknown: {
    action: "none",
    message: "Инструкции не найдены",
  },
  workdir_not_allowed: {
    action: "none",
    message: "Рабочий каталог вне разрешённых корней",
  },
  group_not_found: {
    action: "none",
    message: "Группа не найдена",
  },
  group_name_taken: {
    action: "none",
    message: "Группа с таким именем уже существует",
  },
  conversation_deleted: {
    action: "none",
    message: "Сессия удалена",
  },
  conversation_live: {
    action: "stop_process",
    message: "Сначала остановите сессию",
  },
  archive_active: {
    action: "stop_process",
    message: "В сессии есть активная работа",
  },
  attachment_not_ready: {
    action: "check_acceptance",
    message: "Вложение ещё не готово",
  },
  attachment_not_uploadable: {
    action: "none",
    message: "Загрузка вложения уже завершена или прекращена",
  },
  attachment_unsupported: {
    action: "none",
    message: "Харнесс не принимает такое вложение",
  },
  attachment_unavailable: {
    action: "none",
    message: "Вложение больше недоступно",
  },
  offset_mismatch: {
    action: "none",
    message: "Смещение загрузки не совпадает с принятым",
  },
  upload_in_progress: {
    action: "none",
    message: "Вложение уже загружается",
  },
  checksum_mismatch: {
    action: "none",
    message: "Содержимое не совпало с контрольной суммой",
  },
  attachment_conflict: {
    action: "none",
    message: "Вложение уже завершено с другой контрольной суммой",
  },
  preset_not_found: {
    action: "reload",
    message: "Инструкции не найдены",
  },
  preset_builtin: {
    action: "none",
    message: "Встроенные инструкции нельзя изменить или удалить",
  },
  preset_revision_conflict: {
    action: "reload",
    message: "Инструкции уже изменены",
  },
  preset_title_taken: {
    action: "none",
    message: "Инструкции с таким названием уже есть",
  },
  preset_limit_reached: {
    action: "none",
    message: "Слишком много инструкций",
  },
  preset_invalid: {
    action: "none",
    message: "Некорректные инструкции",
  },
  spawn_whitelist_invalid: {
    action: "none",
    message: "Некорректный белый список",
  },
  not_delegated: {
    action: "none",
    message: "Сессия не запущена агентом",
  },
  worktree_removed: {
    action: "none",
    message: "Рабочая копия уже убрана",
  },
  worktree_not_removed: {
    action: "none",
    message: "Сначала примите результат: рабочая копия ещё на машине",
  },
};

export interface DescribedError {
  action: ErrorAction;
  actionLabel: string | null;
  message: string;
  correlationId?: string;
}

/** Текст для экрана: сообщения хаба и клиента уже по-русски, а коды латиницей владельцу не показываем. */
export function errorText(err: unknown, fallback: string): string {
  if (err instanceof ApiError) return err.message;
  if (err instanceof Error && /[А-Яа-яЁё]/.test(err.message)) return err.message;
  return err instanceof NetworkError ? "Нет связи с хабом" : fallback;
}

export function describeError(err: ApiError | unknown): DescribedError {
  if (err instanceof AuthRequired) {
    return {
      action: "sign_in_again",
      actionLabel: ACTION_LABEL.sign_in_again,
      message: "Требуется вход через сервис авторизации",
    };
  }

  if (err instanceof NetworkError) {
    return {
      action: "restore_connection",
      actionLabel: ACTION_LABEL.restore_connection,
      message: "Нет связи с хабом",
    };
  }

  if (err instanceof ApiError) {
    const descriptor = ERROR_TABLE[err.code];
    const fromRetry: ErrorAction | undefined =
      err.retry === "safe_same_key"
        ? "retry_same_key"
        : err.retry === "explicit_new_operation"
          ? "new_operation"
          : undefined;
    const action = fromRetry ?? descriptor?.action ?? "none";
    if (descriptor || fromRetry) {
      return {
        action,
        actionLabel: ACTION_LABEL[action],
        message: descriptor?.message ?? err.message,
        correlationId: err.correlationId,
      };
    }

    return {
      action: "none",
      actionLabel: null,
      message: `Непредвиденная ошибка хаба, код запроса ${err.correlationId}`,
      correlationId: err.correlationId,
    };
  }

  return {
    action: "none",
    actionLabel: null,
    message: "Непредвиденная ошибка клиентского приложения",
  };
}
