import { rubles } from "#lib/format/money.js";
import { fmt } from "#lib/format/ru.js";
import { profileLines, type ProfileLine } from "#lib/screens/machines/cloudModel.js";

/** Полезная нагрузка карточки `cloud_window`: так её кладёт хаб (snake_case, копейки). */
export interface CloudCardPayload {
  source?: string;
  reason?: string;
  hours?: number;
  hard_cap_minutes?: number;
  hourly_kopecks?: number;
  reservation_kopecks?: number;
  remaining_kopecks?: number;
  over_budget?: boolean;
  expires_at?: string;
  profiles?: Parameters<typeof profileLines>[0];
}

export interface CloudCardView {
  title: string;
  sourceText: string;
  reason: string;
  hoursText: string;
  hourlyText: string;
  reservationText: string;
  remainingText: string;
  overBudget: boolean;
  expiresAt: string | null;
  profiles: ProfileLine[];
}

function sourceWord(source: string | undefined): string {
  switch (source) {
    case "agent":
      return "Предложил агент";
    case "owner":
      return "Предложили вы";
    case "hub":
      return "Предложил хаб: облачная работа ждёт";
    case "repriced":
      return "Пересчитано после смены цены или бюджета";
    default:
      return "Предложение облачного окна";
  }
}

export function cloudWindowView(payload: unknown): CloudCardView {
  const raw = (payload ?? {}) as CloudCardPayload;
  const hours = Number(raw.hours);
  const validHours = Number.isFinite(hours) && hours > 0;
  const money = (value: unknown): string =>
    typeof value === "number" && Number.isFinite(value) ? rubles(value) : "—";
  return {
    title: validHours ? `Облачное окно на ${hours} ч` : "Облачное окно",
    sourceText: sourceWord(raw.source),
    reason: typeof raw.reason === "string" ? raw.reason.trim() : "",
    hoursText: validHours ? `${hours} ч` : "—",
    hourlyText: money(raw.hourly_kopecks),
    reservationText: money(raw.reservation_kopecks),
    remainingText: money(raw.remaining_kopecks),
    overBudget: raw.over_budget === true,
    expiresAt: typeof raw.expires_at === "string" ? raw.expires_at : null,
    profiles: profileLines(Array.isArray(raw.profiles) ? raw.profiles : [])
  };
}

export type OutcomeTone = "done" | "warning" | "danger" | "quiet";

export interface CloudOutcome {
  word: string;
  tone: OutcomeTone;
  note: string;
}

/** Итог карточки словами владельца: коды хаба на экран не попадают. */
export function cloudOutcome(reason: string | undefined): CloudOutcome {
  switch (reason) {
    case "allowed":
    case "applied":
      return { word: "Применено", tone: "done", note: "" };
    case "declined":
      return { word: "Отклонено", tone: "quiet", note: "Окно не запускалось." };
    case "expired":
      return {
        word: "Срок истёк",
        tone: "quiet",
        note: "Карточка живёт полчаса; окно не запускалось."
      };
    case "card_outdated":
      return {
        word: "Выпущена новая карточка",
        tone: "warning",
        note: "Цена или бюджет изменились, пока карточка была открыта. Решение — по новой карточке."
      };
    case "live_window_exists":
      return {
        word: "Окно уже запущено",
        tone: "warning",
        note: "Одновременно работает одно облачное окно."
      };
    case "provider_blocked":
      return {
        word: "Провайдер заблокирован",
        tone: "danger",
        note: "Timeweb требует предоплату: новые окна не создаются, пока блок не снят в настройках."
      };
    case "over_budget":
      return {
        word: "Не хватает бюджета",
        tone: "warning",
        note: "Резерв больше остатка месяца, а согласия сверх бюджета у этой карточки нет."
      };
    case "pool_exhausted":
      return {
        word: "Нет свободных адресов",
        tone: "danger",
        note: "Все локальные адреса или порты роутера заняты окнами."
      };
    case "provider_unavailable":
      return { word: "Провайдер недоступен", tone: "danger", note: "Окно не создавалось." };
    case "price_error":
      return { word: "Цена не посчиталась", tone: "danger", note: "Окно не создавалось." };
    case "invalid_request":
      return { word: "Неверные данные карточки", tone: "danger", note: "Окно не создавалось." };
    case "cloud_disabled":
      return { word: "Облако не настроено", tone: "quiet", note: "Окно не создавалось." };
    case "job_failed":
      return {
        word: "Решение не применилось",
        tone: "danger",
        note: "Хаб не смог довести решение до окна. Попробуйте ещё раз."
      };
    default:
      return { word: "Не применено", tone: "quiet", note: "" };
  }
}

/** Сколько живёт карточка: полчаса от создания, дальше окна не будет. */
export function cloudCardLifetime(expiresAt: string | null, now: number): string {
  if (!expiresAt) return "";
  const at = Date.parse(expiresAt);
  if (!Number.isFinite(at)) return "";
  const left = at - now;
  if (left <= 0) return "Срок карточки истёк";
  const minutes = Math.ceil(left / 60_000);
  if (minutes < 1) return "Карточка истекает меньше чем через минуту";
  return `Карточка живёт ещё ${minutes} ${fmt.plural(minutes, ["минуту", "минуты", "минут"])}`;
}

/** Текст баннера оболочки: одна строка, без отсчёта, чтобы диктор не читал её каждую минуту. */
export function cloudBannerText(payload: unknown): string {
  const view = cloudWindowView(payload);
  const over = view.overBudget ? " · сверх бюджета" : "";
  return `Облачное окно ждёт решения · ${view.hoursText} · резерв ${view.reservationText}${over}`;
}
