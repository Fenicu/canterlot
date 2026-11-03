import type { Tab } from "./grouping";

export const LIST_TAB_KEY = "canterlot.list-tab";

const KINDS = new Set<Tab["kind"]>(["all", "pinned", "group", "archive"]);

function parse(raw: string | null): Tab | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown> | null;
    if (!parsed || typeof parsed !== "object") return null;
    const kind = parsed.kind as Tab["kind"];
    if (!KINDS.has(kind)) return null;
    if (kind !== "group") return { kind } as Tab;
    return typeof parsed.id === "string" && parsed.id
      ? { kind: "group", id: parsed.id }
      : null;
  } catch {
    return null;
  }
}

function save(tab: Tab): void {
  localStorage.setItem(LIST_TAB_KEY, JSON.stringify(tab));
}

/** Последняя вкладка устройства; битое значение сразу заменяется на «Все». */
export function readTab(): Tab {
  const raw = localStorage.getItem(LIST_TAB_KEY);
  const tab = parse(raw);
  if (tab) return tab;
  if (raw !== null) save({ kind: "all" });
  return { kind: "all" };
}

export function writeTab(tab: Tab): void {
  save(tab);
}

/** Удалённая группа возвращает «Все» и переписывает выбор. */
export function resolveTab(tab: Tab, groups: readonly { id: string }[]): Tab {
  if (tab.kind !== "group" || groups.some((group) => group.id === tab.id))
    return tab;
  save({ kind: "all" });
  return { kind: "all" };
}
