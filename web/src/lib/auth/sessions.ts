import { fmt } from "#lib/format/ru.js";
import type { OwnerSession, SessionLocation } from "./api.js";

export const IDLE_OPTIONS = [
  { value: "7", label: "1 неделя" },
  { value: "30", label: "1 месяц" },
  { value: "90", label: "3 месяца" },
  { value: "180", label: "6 месяцев" },
  { value: "365", label: "1 год" },
];

const METHODS: Record<string, string> = {
  password: "вход по паролю",
  passkey: "вход ключом доступа",
  qr: "вход по QR",
  recovery: "вход по ссылке восстановления",
};

export function methodText(method: string): string {
  return METHODS[method] ?? "вход";
}

/** «Москва, Россия», «локальная сеть» или ничего, если базы мест на хабе нет. */
export function placeText(location: SessionLocation | null | undefined): string | null {
  if (!location) return null;
  if (location.local) return "локальная сеть";
  const parts = [location.city, location.country ?? location.region].filter((part): part is string => !!part);
  return parts.length ? parts.join(", ") : null;
}

export function ipText(session: Pick<OwnerSession, "last_ip" | "created_ip">): string {
  return session.last_ip ?? session.created_ip ?? "IP неизвестен";
}

export function sessionTitle(session: Pick<OwnerSession, "name" | "label">): string {
  return session.name || session.label || "Неизвестное устройство";
}

/** «был(а) в сети 5 мин назад». */
export function seenText(iso: string, now: number): string {
  const at = Date.parse(iso);
  const ago = Math.max(0, now - at);
  if (ago < 60_000) return "был(а) в сети только что";
  if (ago < 3_600_000) return `был(а) в сети ${Math.floor(ago / 60_000)} мин назад`;
  if (ago < 86_400_000) return `был(а) в сети ${Math.floor(ago / 3_600_000)} ч назад`;
  return `был(а) в сети ${fmt.stamp(at, now)}`;
}

/** Склонение: 1 сеанс, 2 сеанса, 5 сеансов. */
export function plural(n: number, one: string, few: string, many: string): string {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (mod10 === 1 && mod100 !== 11) return one;
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return few;
  return many;
}

/** Живые сеансы, вход которых подтвердил этот, и подтверждённые ими дальше по цепочке. */
export function cascadeOf(id: string, sessions: OwnerSession[]): OwnerSession[] {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  const found = new Map<string, OwnerSession>();
  const queue = [...(byId.get(id)?.approved ?? [])];
  while (queue.length) {
    const next = queue.shift()!;
    if (next === id || found.has(next)) continue;
    const session = byId.get(next);
    if (!session) continue;
    found.set(next, session);
    queue.push(...session.approved);
  }
  return [...found.values()];
}

/** «Также будут завершены 2 сеанса, вход которых подтвердило это устройство: Pixel 9, Firefox». */
export function cascadeText(cascade: OwnerSession[]): string {
  const n = cascade.length;
  if (n === 0) return "";
  const names = cascade.map(sessionTitle).join(", ");
  return n === 1
    ? `Также будет завершён 1 сеанс, вход которого подтвердило это устройство: ${names}`
    : `Также будут завершены ${n} ${plural(n, "сеанс", "сеанса", "сеансов")}, вход которых подтвердило это устройство: ${names}`;
}
