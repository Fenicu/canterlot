/** Строка каталога, пришедшая из отчёта машины. */
export interface SlashItem {
  name: string;
  description: string;
  kind: string;
  argument_hint: string;
}

const MAX_SHOWN = 12;

/**
 * Пока поле начинается с `/` и в нём нет пробела, возвращаем набранный запрос
 * (без самого слеша). Как только появился пробел или slash перестал быть
 * первым символом, меню команд закрывается.
 */
export function slashQuery(text: string): string | null {
  if (!text.startsWith('/')) return null;
  const rest = text.slice(1);
  if (/\s/.test(rest)) return null;
  return rest;
}

/** Сначала совпадения по началу имени, затем по подстроке; каждое имя один раз. */
export function filterSlashCatalog(items: SlashItem[], query: string): SlashItem[] {
  const needle = query.toLowerCase();
  const prefixes: SlashItem[] = [];
  const substrings: SlashItem[] = [];
  const seen = new Set<string>();
  for (const item of items) {
    if (seen.has(item.name)) continue;
    const name = item.name.toLowerCase();
    if (name.startsWith(needle)) {
      seen.add(item.name);
      prefixes.push(item);
    } else if (name.includes(needle)) {
      seen.add(item.name);
      substrings.push(item);
    }
  }
  return [...prefixes, ...substrings].slice(0, MAX_SHOWN);
}

/** Вставка выбранной команды: только текст в поле, отправки не происходит. */
export function insertSlash(name: string): string {
  return `/${name} `;
}
