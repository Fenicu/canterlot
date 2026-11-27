import type { components } from "#lib/api/generated/openapi.js";

export type PendingMachine = components["schemas"]["PendingMachineView"];

/** Код приходит группами по четыре; вставка из буфера и переносы могут их сбить. */
export function groupCode(code: string): string {
  const plain = code.toUpperCase().replace(/[^0-9A-Z]/g, "");
  const groups: string[] = [];
  for (let index = 0; index < plain.length; index += 4) {
    groups.push(plain.slice(index, index + 4));
  }
  return groups.join("-");
}

/** `null` в ответе хаба значит, что код уже обменян, но воркер ещё не подключился. */
export function codeExpired(
  item: Pick<PendingMachine, "code_expires_at">,
  now: number
): boolean {
  if (!item.code_expires_at) return false;
  const expires = Date.parse(item.code_expires_at);
  return Number.isFinite(expires) && expires <= now;
}

/** Что показать рядом с ожидающей машиной: тревогу истёкшего кода или обменянный. */
export function codeHint(
  item: Pick<PendingMachine, "code_expires_at">,
  now: number
): string {
  if (!item.code_expires_at) return "Код обменян";
  if (!Number.isFinite(Date.parse(item.code_expires_at))) return "";
  return codeExpired(item, now) ? "Код истёк" : "";
}

/** Хаб отдаёт ожидающие машины старыми первыми; владельцу удобнее видеть свежие сверху. */
export function sortPending(items: readonly PendingMachine[]): PendingMachine[] {
  return [...items].sort((left, right) => {
    return Date.parse(right.created_at) - Date.parse(left.created_at);
  });
}
