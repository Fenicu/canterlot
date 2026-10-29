import type { Counts, ListRow } from "#lib/stores/list.svelte.js";

export type Tab =
  | { kind: "all" }
  | { kind: "pinned" }
  | { kind: "group"; id: string }
  | { kind: "archive" };

export interface Group {
  id: string;
  name: string;
  position: number;
}

export interface Section {
  id: string;
  title: string;
  count: number;
  items: ListRow[];
  collapsed: boolean;
}

export function buildSections(
  items: ListRow[],
  groups: Group[],
  tab: Tab,
  collapsed: ReadonlySet<string>,
): Section[] {
  const ordered = [...groups].sort(
    (a, b) => a.position - b.position || a.name.localeCompare(b.name, "ru"),
  );
  const active = items.filter((item) => !item.archived);
  if (tab.kind === "archive") {
    return [
      section(
        "archive",
        "Архив",
        items.filter((item) => item.archived),
        collapsed,
      ),
    ];
  }
  if (tab.kind === "pinned") {
    return [
      section(
        "pinned",
        "Закреплённые",
        active.filter((item) => item.pinned),
        collapsed,
      ),
    ];
  }
  if (tab.kind === "group") {
    const group = ordered.find((item) => item.id === tab.id);
    const members = active.filter((item) => item.groups.includes(tab.id));
    return [section(tab.id, group?.name ?? "Группа", members, collapsed)];
  }
  const used = new Set<string>();
  const sections: Section[] = [];
  const pinned = active.filter((item) => item.pinned);
  for (const item of pinned) used.add(item.id);
  if (pinned.length > 0)
    sections.push(section("pinned", "Закреплённые", pinned, collapsed));
  for (const group of ordered) {
    const members = active.filter(
      (item) => !used.has(item.id) && firstGroup(item, ordered) === group.id,
    );
    for (const item of members) used.add(item.id);
    if (members.length > 0)
      sections.push(section(group.id, group.name, members, collapsed));
  }
  const rest = active.filter((item) => !used.has(item.id));
  if (rest.length > 0)
    sections.push(section("none", "Без группы", rest, collapsed));
  return sections;
}

export function tabCounts(
  counts: Counts,
  groups: { id: string; name: string }[],
): { tab: Tab; label: string; count: number }[] {
  const byId = new Map(counts.groups.map((group) => [group.id, group.count]));
  return [
    { tab: { kind: "all" }, label: "Все", count: counts.all },
    { tab: { kind: "pinned" }, label: "Закреплённые", count: counts.pinned },
    ...groups.map((group) => ({
      tab: { kind: "group" as const, id: group.id },
      label: group.name,
      count: byId.get(group.id) ?? 0,
    })),
    { tab: { kind: "archive" }, label: "Архив", count: counts.archived },
  ];
}

function firstGroup(item: ListRow, groups: Group[]): string | null {
  for (const group of groups) {
    if (item.groups.includes(group.id)) return group.id;
  }
  return null;
}

function section(
  id: string,
  title: string,
  items: ListRow[],
  collapsed: ReadonlySet<string>,
): Section {
  return {
    id,
    title,
    count: items.length,
    items,
    collapsed: collapsed.has(id),
  };
}
