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

/**
 * Полный правимый снимок версии для `PUT`: вид, тема, текст, уровень с проектом и срок.
 * Хаб кладёт в версию все эти поля; отсутствующий срок значит «без срока», и он снимается.
 */
export function revisionSnapshot(revision: Pick<MemoryRevision, "kind" | "subject" | "body" | "project" | "valid_until">) {
  return {
    kind: revision.kind,
    subject: revision.subject,
    body: revision.body,
    level: revision.project ? "project" : "domain",
    project: revision.project ?? undefined,
    valid_until: revision.valid_until ?? null
  };
}

export const RESTORE_NOTE = (version: number): string =>
  `Вернутся тема, вид, уровень, срок и текст версии ${version}. Это станет новой версией записи, прежняя история сохранится`;

/** Кто менял запись: `{kind: owner | agent | import, ...}` из `updated_by`. */
export function actorLabel(actor: unknown): string {
  const value = (actor ?? {}) as { kind?: string; harness?: string };
  if (value.kind === "owner") return "владельцем";
  if (value.kind === "agent") return value.harness ? `агентом (${value.harness})` : "агентом";
  if (value.kind === "import") return "импортом";
  return "неизвестно кем";
}

/** «Изменено владельцем, 1 нояб., 09:05»: кто и когда менял запись, одной короткой строкой. */
export function changedLine(entry: { updated_by: unknown; updated_at: string }): string {
  const when = new Date(entry.updated_at).toLocaleString("ru-RU", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  return `Изменено ${actorLabel(entry.updated_by)}, ${when}`;
}

/** Запоминает проекты загруженных записей: список фильтра не сужается вместе с выборкой. */
export function collectProjects(seen: Map<string, string>, entries: { project?: string | null; project_label?: string | null }[]): void {
  for (const entry of entries) {
    if (entry.project) seen.set(entry.project, entry.project_label ?? entry.project);
  }
}

export function projectOptions(seen: Map<string, string>): { value: string; label: string }[] {
  const rows = [...seen].map(([value, label]) => ({ value, label })).sort((a, b) => a.label.localeCompare(b.label, "ru") || a.value.localeCompare(b.value));
  return [{ value: "", label: "Все проекты" }, ...rows];
}

export function kindOptions(): { value: string; label: string }[] {
  return [{ value: "", label: "Все виды" }, ...KINDS];
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

export const LINK_NOTICE = {
  missing: "Записи нет в этом домене: она удалена или лежит в другом",
  archived: "Запись в архиве: откройте её во вкладке «В архиве»",
  failed: "Не удалось открыть запись по ссылке. Повторите при восстановлении связи"
} as const;

export type LinkedEntry =
  | { kind: "open"; entry: MemoryEntry; domain: MemoryDomain }
  | { kind: "notice"; notice: string; domain?: MemoryDomain };

/**
 * Запись по ссылке из карточки ленты. Домен в ссылке известен не всегда (поиск агента его не
 * называет): без подсказки запись ищется в обоих доменах, с подсказкой — только в названном.
 * Любая неудача — короткое пояснение над списком, а не страница ошибки.
 */
export async function resolveLinkedEntry(
  get: (id: string, domain: MemoryDomain) => Promise<MemoryEntry>,
  id: string,
  hint: string | null
): Promise<LinkedEntry> {
  const domains: MemoryDomain[] = hint === "work" || hint === "personal" ? [hint] : ["work", "personal"];
  for (const domain of domains) {
    try {
      const entry = await get(id, domain);
      if (entry.status === "archived") return { kind: "notice", notice: LINK_NOTICE.archived, domain };
      return { kind: "open", entry, domain };
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) continue;
      return { kind: "notice", notice: LINK_NOTICE.failed };
    }
  }
  return { kind: "notice", notice: LINK_NOTICE.missing };
}

/** Срок действия: поле даты в конце суток по UTC; пустое поле снимает срок. */
export function dateToValidUntil(date: string): string | null {
  return /^\d{4}-\d{2}-\d{2}$/.test(date) ? `${date}T23:59:59Z` : null;
}

export function validUntilToDate(stamp: string | null | undefined): string {
  return stamp ? stamp.slice(0, 10) : "";
}
