// Логика экрана пресетов: сортировка, подписи происхождения, счётчик байтов.
// Текст пресетов здесь не хранится и не печатается: только длины и метаданные.
import type { components } from "#lib/api/generated/openapi.js";

export type PresetSummaryLike = components["schemas"]["PresetSummary"];
export type PresetOriginLike = components["schemas"]["PresetOrigin"];

// Лимит текста пресета: тот же MAX_PRESET_TEXT_BYTES, что и в хабе.
export const PRESET_TEXT_LIMIT = 65_536;

// Встроенные пресеты всегда первые: «Без инструкций», затем «Как у владельца».
// Пользовательские идут дальше по названию без учёта регистра.
export function sortPresets(items: PresetSummaryLike[]): PresetSummaryLike[] {
  const rank = (item: PresetSummaryLike): number =>
    item.kind === "clean" ? 0 : item.kind === "owner" ? 1 : 2;
  return [...items].sort((a, b) => {
    const order = rank(a) - rank(b);
    if (order !== 0) return order;
    return a.title.localeCompare(b.title, "ru");
  });
}

function sessionSuffix(origin: PresetOriginLike): string {
  return origin.conversation_title ? ` · «${origin.conversation_title}»` : "";
}

// Подпись происхождения для списка: кто создал и кто правил последним.
export function formatOrigin(item: PresetSummaryLike): string {
  const parts: string[] = [];
  parts.push(item.created.by === "agent" ? `Создан агентом${sessionSuffix(item.created)}` : "Создан владельцем");
  if (item.last_edit && item.last_edit.by === "agent") {
    parts.push(`Изменён агентом${sessionSuffix(item.last_edit)}`);
  }
  return parts.join(" · ");
}

// Плашка «агент»: пресет создан или правлен агентом.
export function isAgentMarked(item: PresetSummaryLike): boolean {
  return item.created.by === "agent" || item.last_edit?.by === "agent";
}

function formatKib(bytes: number): string {
  if (bytes === 0) return "0";
  return (bytes / 1024).toFixed(1).replace(".", ",");
}

// Счётчик текста редактора: «12,0 КиБ из 64 КиБ», флаг превышения лимита.
export function byteCounter(bytes: number): { text: string; over: boolean } {
  return { text: `${formatKib(bytes)} КиБ из 64 КиБ`, over: bytes > PRESET_TEXT_LIMIT };
}

// Длина текста в байтах UTF-8 для счётчика редактора.
export function textBytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

export interface AppliedPresetLike {
  revision?: number | null;
  current_revision?: number | null;
}

// Кнопка «Применить новую версию» видна, когда применённая версия отстала.
export function shouldShowApplyNewVersion(applied: AppliedPresetLike): boolean {
  if (applied.current_revision == null || applied.revision == null) return false;
  return applied.current_revision > applied.revision;
}
