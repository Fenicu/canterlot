// Чистые помощники экрана «Память»: подписи, статусы, разбор отказов хаба и поле срока.
// Никаких обращений к API и Svelte: всё доступно юнит-тестам.
import type { components } from "#lib/api/generated/openapi.js";
import { ApiError } from "#lib/api/http.js";

export type MemoryEntry = components["schemas"]["MemoryEntryView"];
export type MemoryRevision = components["schemas"]["MemoryRevisionView"];
export type MemoryDomain = "work" | "personal";
export type Tone = "ok" | "warn" | "danger" | "neutral";

export const DOMAINS: { value: MemoryDomain; label: string }[] = [
  { value: "personal", label: "Личное" },
  { value: "work", label: "Работа" }
];

export const KINDS: { value: string; label: string }[] = [
  { value: "user", label: "О владельце" },
  { value: "feedback", label: "Замечание" },
  { value: "project", label: "О проекте" },
  { value: "reference", label: "Справка" },
  { value: "decision", label: "Решение" },
  { value: "gotcha", label: "Ловушка" },
  { value: "environment", label: "Окружение" },
  { value: "incident", label: "Инцидент" }
];

export function kindLabel(kind: string): string {
  return KINDS.find((entry) => entry.value === kind)?.label ?? kind;
}

export function levelLabel(entry: { level: string; project_label?: string | null }): string {
  return entry.level === "project" && entry.project_label ? `Проект ${entry.project_label}` : "Весь домен";
}

/** Статус записи словом и тоном: цвет не несёт смысл в одиночку. */
export function entryStatus(entry: { status: string; expired: boolean }): { word: string; tone: Tone } {
  if (entry.status === "archived") return { word: "В архиве", tone: "neutral" };
  if (entry.expired) return { word: "Срок истёк", tone: "warn" };
  return { word: "Активна", tone: "ok" };
}

/** Строка памяти в карточке сессии: домен, «нет памяти» или «конфликт домена». */
export function sessionMemoryLabel(detail: { memory_domain?: string | null; memory_conflict?: boolean }): string {
  if (detail.memory_domain === "work") return "Работа";
  if (detail.memory_domain === "personal") return "Личное";
  return detail.memory_conflict ? "конфликт домена" : "нет памяти";
}

/** Текущая версия записи из отказа `version_conflict`; для любого другого отказа `null`. */
export function conflictVersion(error: unknown): number | null {
  if (!(error instanceof ApiError) || error.code !== "version_conflict") return null;
  return error.details?.current_version ?? null;
}

/** Класс находки из отказа `looks_like_secret`; значение хаб не возвращает. */
export function secretClass(error: unknown): string | null {
  if (!(error instanceof ApiError) || error.code !== "looks_like_secret") return null;
  return error.details?.class ?? null;
}

/** Срок действия: поле даты в конце суток по UTC; пустое поле снимает срок. */
export function dateToValidUntil(date: string): string | null {
  return /^\d{4}-\d{2}-\d{2}$/.test(date) ? `${date}T23:59:59Z` : null;
}

export function validUntilToDate(stamp: string | null | undefined): string {
  return stamp ? stamp.slice(0, 10) : "";
}
