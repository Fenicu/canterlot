// Логика экрана пресетов: сортировка, подписи происхождения, счётчик байтов.
// Текст пресетов здесь не хранится и не печатается: только длины и метаданные.
import type { components } from "#lib/api/generated/openapi.js";
import { fmt } from "#lib/format/ru.js";

export type PresetSummaryLike = components["schemas"]["PresetSummary"];
export type PresetOriginLike = components["schemas"]["PresetOrigin"];

// Лимит текста пресета: тот же MAX_PRESET_TEXT_BYTES, что и в хабе.
export const PRESET_TEXT_LIMIT = 65_536;
// Текст толчка короче инструкции: правило обещает до 4 КиБ (§4.5).
export const PRESET_NUDGE_TEXT_LIMIT = 4_096;

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
export function byteCounter(
  bytes: number,
  limit = PRESET_TEXT_LIMIT,
): { text: string; over: boolean } {
  const kib = limit / 1024;
  const size = Number.isInteger(kib) ? String(kib) : formatKib(limit);
  return { text: `${formatKib(bytes)} КиБ из ${size} КиБ`, over: bytes > limit };
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

export type OwnerFileLike = components["schemas"]["OwnerFileView"];
export type OwnerProjectLike = components["schemas"]["OwnerMemoryProjectView"];
export type OwnerOverviewLike = components["schemas"]["OwnerOverviewView"];
export type OwnerContentLike = components["schemas"]["OwnerFileContent"];

// Подписи видов для листа «Как у владельца»: путь и роль файла.
export const OWNER_KIND_META: Record<string, { harness: string; title: string; note: string }> = {
  claude_user: {
    harness: "claude",
    title: "Claude Code · пользовательский",
    note: "~/.claude/CLAUDE.md для всех проектов",
  },
  claude_home: {
    harness: "claude",
    title: "Claude Code · домашний",
    note: "~/CLAUDE.md действует для проектов под домом",
  },
  codex_global: {
    harness: "codex",
    title: "Codex · глобальный",
    note: "$CODEX_HOME/AGENTS.md, по умолчанию ~/.codex/AGENTS.md",
  },
  codex_override: {
    harness: "codex",
    title: "Codex · переопределение",
    note: "AGENTS.override.md читается вместо AGENTS.md",
  },
  opencode_global: {
    harness: "opencode",
    title: "OpenCode · глобальный",
    note: "~/.config/opencode/AGENTS.md",
  },
};

// Названия харнессов те же, что в заголовках карточек файлов.
export const OWNER_HARNESS_TITLES: Record<string, string> = { claude: "Claude Code", codex: "Codex", opencode: "OpenCode" };

// «1 проект», «2 проекта», «5 проектов», «нет проектов».
export function memoryProjects(count: number): string {
  if (count === 0) return "нет проектов";
  const tail = count % 100 >= 11 && count % 100 <= 14 ? 10 : count % 10;
  const word = tail === 1 ? "проект" : tail >= 2 && tail <= 4 ? "проекта" : "проектов";
  return `${count} ${word}`;
}

// Краткий итог по харнессу: сколько файлов найдено и общий объём.
export function ownerHarnessSummary(
  files: OwnerFileLike[],
): { harness: string; title: string; found: number; total: number; text: string }[] {
  const groups = new Map<string, { found: number; total: number }>();
  for (const file of files) {
    const slot = groups.get(file.harness) ?? { found: 0, total: 0 };
    if (file.present) {
      slot.found += 1;
      slot.total += file.size;
    }
    groups.set(file.harness, slot);
  }
  return ["claude", "codex", "opencode"].map((harness) => {
    const slot = groups.get(harness) ?? { found: 0, total: 0 };
    const filesWord =
      slot.found === 1 ? "файл" : slot.found >= 2 && slot.found <= 4 ? "файла" : "файлов";
    return {
      harness,
      title: OWNER_HARNESS_TITLES[harness] ?? harness,
      found: slot.found,
      total: slot.total,
      text: `${slot.found} ${filesWord}, ${formatSize(slot.total)}`,
    };
  });
}

// Размер файла: «0 Б», «1,2 КиБ», «3,4 МиБ».
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} Б`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1).replace(".", ",")} КиБ`;
  return `${(bytes / 1024 / 1024).toFixed(1).replace(".", ",")} МиБ`;
}

// Время изменения для подписи файла, как в остальном приложении: «28 октября, 15:00», год только не текущий.
export function formatOwnerMtime(mtime: string | null | undefined, now = Date.now()): string {
  if (!mtime) return "";
  const date = new Date(mtime);
  if (Number.isNaN(date.getTime())) return "";
  return fmt.stamp(date.getTime(), now);
}
