/** Секрет из ссылки кода входа: только адрес этого же canterlot вида `/qr#…`. */
export function qrSecret(text: string, origin: string): string | null {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return null;
  }
  if (url.origin !== origin || url.pathname !== "/qr" || url.search) return null;
  const secret = url.hash.slice(1);
  return /^[A-Za-z0-9_-]{16,128}$/.test(secret) ? secret : null;
}

export function cameraErrorText(error: unknown): string {
  const name = error instanceof Error || error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError")
    return "Нет доступа к камере. Разрешите камеру для canterlot в настройках браузера";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "Камера не найдена";
  if (name === "NotReadableError" || name === "AbortError") return "Камера занята другим приложением или недоступна";
  return "Не удалось включить камеру";
}

/** Фрагмент из адресной строки: читается один раз и сразу убирается из адреса и истории. */
export function takeFragment(replace: () => void): string {
  const fragment = location.hash.slice(1);
  if (location.hash) replace();
  return fragment;
}
