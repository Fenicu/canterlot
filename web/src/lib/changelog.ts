/**
 * История изменений: разбор `CHANGES.rst` и выбор того, что человек ещё не
 * видел.
 *
 * Источник один — тот же файл, что читают в репозитории. Отдельная копия для
 * интерфейса разъехалась бы с ним на второй же неделе.
 */
import { fmt } from "#lib/format/ru.js";
import { compareVersions } from "#lib/version.js";
import raw from "../../../CHANGES.rst?raw";

export { compareVersions };

export interface ChangelogSection {
  /** Название раздела как в файле: «Добавлено», «Удалено» и так далее. */
  title: string;
  items: string[];
}

export interface ChangelogEntry {
  version: string;
  /** Дата как в файле; пусто, если её не указали. */
  date: string;
  sections: ChangelogSection[];
  /** Текст без разделов — им открывается самая первая запись. */
  preamble: string[];
}

const VERSION_LINE = /^(\d+\.\d+(?:\.\d+)?)(?:\s+—\s+(.+))?$/;

function isUnderline(line: string | undefined, char: string): boolean {
  return !!line && line.length >= 3 && line === char.repeat(line.length);
}

/**
 * Разбор нужного подмножества reStructuredText: заголовок версии подчёркнут
 * дефисами, раздел — тильдами, пункт начинается с `- ` и может продолжаться
 * на следующих строках с отступом.
 */
export function parseChangelog(text: string = raw): ChangelogEntry[] {
  const lines = text.split("\n");
  const entries: ChangelogEntry[] = [];
  let entry: ChangelogEntry | null = null;
  let section: ChangelogSection | null = null;
  let bullet: string[] | null = null;

  const flushBullet = (): void => {
    if (!bullet) return;
    const value = bullet.join(" ").replace(/\s+/g, " ").trim();
    if (value) {
      if (section) section.items.push(value);
      else if (entry) entry.preamble.push(value);
    }
    bullet = null;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? "";
    const next = lines[i + 1];

    const version = line.trim().match(VERSION_LINE);
    if (version && isUnderline(next?.trim(), "-")) {
      flushBullet();
      entry = { version: version[1]!, date: version[2]?.trim() ?? "", sections: [], preamble: [] };
      section = null;
      entries.push(entry);
      i++;
      continue;
    }

    if (entry && line.trim() && isUnderline(next?.trim(), "~")) {
      flushBullet();
      section = { title: line.trim(), items: [] };
      entry.sections.push(section);
      i++;
      continue;
    }

    if (!entry) continue;

    if (line.startsWith("- ")) {
      flushBullet();
      bullet = [line.slice(2).trim()];
      continue;
    }
    if (bullet && line.startsWith("  ") && line.trim()) {
      bullet.push(line.trim());
      continue;
    }
    if (!line.trim()) {
      flushBullet();
      continue;
    }
    // Обычный абзац первой записи (в ней разделов нет).
    if (!section && !line.startsWith(".. ") && !line.startsWith("=")) {
      entry.preamble.push(line.trim());
    }
  }
  flushBullet();
  return entries;
}

/**
 * Записи, вышедшие после версии `seen`, самая свежая первой.
 *
 * `seen` пустой — человек здесь впервые (или чистил хранилище): показывать
 * ему всю историю незачем, окно «Что нового» для этого не предназначено.
 */
export function entriesSince(
  seen: string | null,
  current: string,
  entries: ChangelogEntry[] = parseChangelog()
): ChangelogEntry[] {
  if (!seen) return [];
  if (compareVersions(seen, current) >= 0) return [];
  return entries
    .filter((entry) => compareVersions(entry.version, seen) > 0 && compareVersions(entry.version, current) <= 0)
    .sort((a, b) => compareVersions(b.version, a.version));
}

/** Дата записи «02.11.2026» в виде, как в остальном приложении: «2 ноября», с годом, если он не текущий. */
export function changelogDate(text: string, now = Date.now()): string {
  const match = /^(\d{2})\.(\d{2})\.(\d{4})$/.exec(text.trim());
  if (!match) return text;
  const ts = new Date(Number(match[3]), Number(match[2]) - 1, Number(match[1]), 12).getTime();
  return fmt.date(ts, now);
}
