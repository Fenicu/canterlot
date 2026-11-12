import { errorText } from "#lib/api/errors.js";
import { ApiError } from "#lib/api/http.js";
import type { SignOut } from "#lib/api/http.js";

/** «Слишком много попыток. Повторите через 4 мин»: минуты вверх, не меньше одной. */
export function lockText(seconds: number | undefined): string {
  const minutes = Math.max(1, Math.ceil((seconds ?? 60) / 60));
  return `Слишком много попыток. Повторите через ${minutes} мин`;
}

export function signInErrorText(error: unknown, fallback = "Не удалось войти"): string {
  if (error instanceof ApiError) {
    if (error.code === "invalid_credentials") return "Неверный логин или пароль";
    if (error.code === "too_many_attempts") return lockText(error.details?.retry_after_seconds);
    if (error.code === "wrong_password") return "Неверный пароль";
    if (error.code === "passkey_rejected") return "Ключ доступа не подошёл";
    if (error.code === "no_passkeys") return "Ключей доступа пока нет";
    if (error.code === "passkeys_unavailable") return "Ключи доступа на этом хабе недоступны";
  }
  return errorText(error, fallback);
}

/** Подзаголовок страницы входа: почему она открылась. */
export function causeText(cause: SignOut | null, wiped: boolean): string {
  if (!cause) return "";
  if (cause.wipe)
    return wiped
      ? "Сеанс завершён с другого устройства или сервером. Данные на этом устройстве удалены"
      : "Сеанс завершён. Удаляю данные на устройстве…";
  if (cause.code === "session_expired" || cause.reason === "idle") return "Сеанс завершён из-за бездействия. Войдите снова";
  if (cause.reason === "limit") return "Сеанс завершён: открыто слишком много сеансов. Войдите снова";
  if (cause.reason === "logout") return "Вы вышли из canterlot";
  return "";
}

/** Сразу после входа: приложение заново загружается уже с сеансом. */
export function afterSignIn(path?: string): void {
  window.location.replace(path ?? `${location.pathname}${location.search}`);
}
