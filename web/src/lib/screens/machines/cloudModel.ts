import type { components } from "#lib/api/generated/openapi.js";
import { rubles } from "#lib/format/money.js";
import { fmt } from "#lib/format/ru.js";

export type CloudWindow = components["schemas"]["CloudWindowView"];
export type CloudWindows = components["schemas"]["WindowList"];
export type CloudBudget = components["schemas"]["BudgetView"];
export type CloudDeleteOutcome = components["schemas"]["DeleteRequested"];

/** Окно ещё оплачивается: от вызова создания до подтверждённого удаления. */
export const LIVE_WINDOW_STATES = new Set([
  "create_intent",
  "create_unknown",
  "identified",
  "ready",
  "draining",
  "deleting"
]);

export function isLiveWindow(window: Pick<CloudWindow, "state">): boolean {
  return LIVE_WINDOW_STATES.has(window.state);
}

/** Живое окно одно (его держит хаб); рядом — последние законченные. */
export function splitWindows(items: readonly CloudWindow[]): {
  live: CloudWindow | null;
  ended: CloudWindow[];
} {
  return {
    live: items.find(isLiveWindow) ?? null,
    ended: items.filter((window) => !isLiveWindow(window))
  };
}

export function windowStateWord(state: string): string {
  switch (state) {
    case "create_intent":
      return "Создаётся";
    case "create_unknown":
      return "Создание без ответа";
    case "identified":
      return "Машина создана";
    case "ready":
      return "Готов";
    case "draining":
      return "Завершается";
    case "deleting":
      return "Удаляется";
    case "ended":
      return "Завершено";
    case "failed":
      return "Не создалось";
    case "gone":
      return "Пропало";
    default:
      return "Состояние неизвестно";
  }
}

/** Сколько окно живёт: от создания строки до сейчас. */
export function windowAge(window: Pick<CloudWindow, "created_at">, now: number): string {
  const started = Date.parse(window.created_at);
  if (!Number.isFinite(started)) return "";
  return fmt.durationCoarse(Math.max(0, now - started));
}

/** Остаток до жёсткого предела; прошлый предел называется словами, а не отрицательным числом. */
export function windowDeadline(
  window: Pick<CloudWindow, "hard_cap_at">,
  now: number
): string | null {
  if (!window.hard_cap_at) return null;
  const at = Date.parse(window.hard_cap_at);
  if (!Number.isFinite(at)) return null;
  const left = at - now;
  return left > 0
    ? `до предела ${fmt.durationCoarse(left)}`
    : "предел прошёл, окно дорабатывает и удаляется";
}

/** Итог законченного окна или расход живого — то, что хаб посчитал сам. */
export function windowCost(window: CloudWindow): number {
  return window.final_cost_kopecks ?? window.cost_so_far_kopecks;
}

export function windowCostLine(window: CloudWindow): string {
  if (isLiveWindow(window)) {
    return `Расход ${rubles(window.cost_so_far_kopecks)} из резерва ${rubles(window.reservation_kopecks)}`;
  }
  return `Итог ${rubles(window.final_cost_kopecks ?? 0)}`;
}

export function windowPriceLine(window: Pick<CloudWindow, "price_hourly_kopecks">): string {
  return `${rubles(window.price_hourly_kopecks)}/ч`;
}

/** Что владельцу делать с ошибкой службы окон. */
export function windowErrorText(code: string | null | undefined): string | null {
  if (!code) return null;
  return `Служба окон сообщила об ошибке: ${code}`;
}

/** Блок провайдера: почему новые окна не создаются. */
export function blockText(code: string | null | undefined): string | null {
  if (!code) return null;
  return `Провайдер остановил создание окон (код ${code})`;
}

/** Почему окно закончилось не по воле владельца: коды Яндекса словами. */
export function windowEndReason(reason: string | null | undefined): string | null {
  switch (reason) {
    case "preempted":
      return "прервана Яндексом";
    case "vm_broken":
      return "машина сломалась";
    default:
      return null;
  }
}

/** Состояние окна словами владельца и, если известно, чем оно кончилось. */
export function windowStateText(window: Pick<CloudWindow, "state" | "delete_reason">): string {
  const reason = windowEndReason(window.delete_reason);
  const word = windowStateWord(window.state);
  return reason ? `${word} · ${reason}` : word;
}

export interface WindowReplacement {
  text: string;
  parentId: string | null;
}

/** Замена прерванного окна: своё начало словами и родитель, если он есть в списке. */
export function windowReplacement(
  window: Pick<CloudWindow, "replaces_window_id">,
  items: readonly CloudWindow[],
  now: number
): WindowReplacement | null {
  if (!window.replaces_window_id) return null;
  const parent = items.find((item) => item.id === window.replaces_window_id);
  if (!parent) return { text: "замена окна", parentId: null };
  const started = Date.parse(parent.created_at);
  return Number.isFinite(started)
    ? { text: `замена окна ${fmt.stamp(started, now)}`, parentId: parent.id }
    : { text: "замена окна", parentId: parent.id };
}

export function deleteOutcomeText(outcome: CloudDeleteOutcome): string {
  switch (outcome) {
    case "deleting":
      return "Удаляю машину. Оплата идёт до подтверждения удаления";
    case "canceled":
      return "Окно отменено: машина ещё не создавалась";
    case "not_yet":
      return "Машины ещё нет в списке провайдера. Удаление повторится, когда она появится";
    case "already_ended":
      return "Окно уже закончилось";
    default:
      return "Окно не найдено";
  }
}

/** Отказ формы запуска: хаб присылает код причины в `details.reason`. */
export function proposalRefusalText(reason: string): string {
  switch (reason) {
    case "live_window_exists":
      return "Облачное окно уже запущено: одновременно работает одно";
    case "open_card_exists":
      return "Открытая карточка уже есть: решите её или дождитесь срока";
    case "invalid_hours":
      return "Выберите от 1 до 4 часов";
    case "cloud_disabled":
      return "Облако не настроено на хабе";
    case "provider_unavailable":
      return "Yandex Cloud не отвечает: цену получить не удалось";
    case "provider_blocked":
      return "Провайдер остановил создание окон: снимите блок в настройках";
    case "price_error":
      return "Не удалось посчитать цену окна";
    default:
      return "Не удалось создать карточку. Повторите при восстановлении связи";
  }
}

export interface ProfileLine {
  key: string;
  text: string;
}

/** Профили окна только для чтения: характеристики и цена часа. */
export function profileLines(
  profiles: readonly CloudBudget["profiles"][number][]
): ProfileLine[] {
  return profiles.map((profile) => ({
    key: `${profile.cores}-${profile.memory_gb}-${profile.disk_gb}`,
    text: `${profile.cores} vCPU · ${profile.memory_gb} ГБ · SSD ${profile.disk_gb} ГБ · ≈${rubles(profile.compute_kopecks_per_hour + profile.disk_kopecks_per_hour)}/ч`
  }));
}
