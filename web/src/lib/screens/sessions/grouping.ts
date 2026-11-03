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

/** Строка списка с вложенными делегированными сессиями. */
export interface TreeNode {
  item: ListRow;
  /** Вложенность под родителем; отступ дальше двойки не растёт. */
  depth: number;
  /** Родитель раскрыт: видно его детей. */
  open: boolean;
  /** Живых сессий в поддереве без самой строки. */
  running: number;
  /** Из живых: ждут ответа владельца, а не работают. */
  waiting: number;
  children: TreeNode[];
}

export interface Section {
  id: string;
  title: string;
  count: number;
  items: TreeNode[];
  collapsed: boolean;
}

/** Явный выбор владельца по родителям: свернул или раскрыл поверх умолчания. */
export interface KidsChoice {
  collapsed: ReadonlySet<string>;
  expanded: ReadonlySet<string>;
}

const noKids: KidsChoice = {
  collapsed: new Set<string>(),
  expanded: new Set<string>(),
};

/** Живой ход: сессия что-то делает или ждёт ответа прямо сейчас. */
export function childLive(item: ListRow): boolean {
  const state = item.activity.state;
  if (state === "working" || state === "waiting_for_user" || state === "stalled")
    return true;
  if (state !== "idle") return false;
  const evidence = item.activity.evidence as
    | { background_work?: unknown }
    | undefined;
  return typeof evidence?.background_work === "number" && evidence.background_work > 0;
}

export function buildSections(
  items: ListRow[],
  groups: Group[],
  tab: Tab,
  collapsed: ReadonlySet<string>,
  kids: KidsChoice = noKids,
): Section[] {
  const ordered = [...groups].sort(
    (a, b) => a.position - b.position || a.name.localeCompare(b.name, "ru"),
  );
  const active = items.filter((item) => !item.archived);
  if (tab.kind === "archive") {
    const archived = items.filter((item) => item.archived);
    return [
      section(
        "archive",
        "Архив",
        nest(archived, new Set(archived.map((item) => item.id)), kids),
        collapsed,
      ),
    ];
  }
  if (tab.kind === "pinned") {
    const pinned = active.filter((item) => item.pinned);
    return [
      section(
        "pinned",
        "Закреплённые",
        nest(active, new Set(pinned.map((item) => item.id)), kids),
        collapsed,
      ),
    ];
  }
  if (tab.kind === "group") {
    const group = ordered.find((item) => item.id === tab.id);
    const members = active.filter((item) => item.groups.includes(tab.id));
    return [
      section(
        tab.id,
        group?.name ?? "Группа",
        nest(active, new Set(members.map((item) => item.id)), kids),
        collapsed,
      ),
    ];
  }
  const roots = nest(active, null, kids);
  const used = new Set<string>();
  const sections: Section[] = [];
  const pinned = roots.filter((node) => node.item.pinned);
  for (const node of pinned) used.add(node.item.id);
  if (pinned.length > 0)
    sections.push(section("pinned", "Закреплённые", pinned, collapsed));
  for (const group of ordered) {
    const members = roots.filter(
      (node) =>
        !used.has(node.item.id) && firstGroup(node.item, ordered) === group.id,
    );
    for (const node of members) used.add(node.item.id);
    if (members.length > 0)
      sections.push(section(group.id, group.name, members, collapsed));
  }
  const rest = roots.filter((node) => !used.has(node.item.id));
  if (rest.length > 0)
    sections.push(section("none", "Без группы", rest, collapsed));
  return sections;
}

/**
 * Дети встают под родителя из того же набора и едут с ним по вкладкам:
 * ребёнок без видимого родителя остаётся обычной строкой, а чужой
 * вкладке невидим вовсе. Счёт и группы считают только верхний уровень.
 */
function nest(
  pool: ListRow[],
  topIds: Set<string> | null,
  kids: KidsChoice,
): TreeNode[] {
  const tops = topIds ?? new Set(pool.map((item) => item.id));
  const nodes = new Map<string, TreeNode>();
  for (const item of pool) {
    nodes.set(item.id, {
      item,
      depth: 0,
      open: false,
      running: 0,
      waiting: 0,
      children: [],
    });
  }
  const roots: TreeNode[] = [];
  const order = [...pool].sort(
    (a, b) =>
      a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id),
  );
  const visible = new Map<string, boolean>();
  const visiting = new Set<string>();
  const parentOf = (node: TreeNode): TreeNode | null =>
    node.item.parent_id ? (nodes.get(node.item.parent_id) ?? null) : null;
  const attachable = (node: TreeNode, parent: TreeNode | null): boolean =>
    parent !== null &&
    parent !== node &&
    !reaches(parent, node.item.id, nodes);
  // Видимость идёт по всему поддереву: внук закреплённого корня едет с ним,
  // хотя сам не закреплён. Петля ссылок не должна прятать строки.
  const shown = (node: TreeNode): boolean => {
    const known = visible.get(node.item.id);
    if (known !== undefined) return known;
    if (visiting.has(node.item.id)) return false;
    visiting.add(node.item.id);
    const parent = parentOf(node);
    const result =
      (parent !== null && attachable(node, parent) && shown(parent)) ||
      tops.has(node.item.id);
    visiting.delete(node.item.id);
    visible.set(node.item.id, result);
    return result;
  };
  for (const item of order) {
    const node = nodes.get(item.id)!;
    const parent = parentOf(node);
    if (parent !== null && attachable(node, parent) && shown(parent)) {
      parent.children.push(node);
      continue;
    }
    if (tops.has(item.id)) roots.push(node);
  }
  for (const node of nodes.values()) {
    node.children.sort(
      (a, b) =>
        a.item.created_at.localeCompare(b.item.created_at) ||
        a.item.id.localeCompare(b.item.id),
    );
  }
  const rootSet = new Set(roots);
  const depthOf = (node: TreeNode, seen: Set<string>): number => {
    if (rootSet.has(node) || seen.has(node.item.id)) return 0;
    seen.add(node.item.id);
    const parent = node.item.parent_id
      ? (nodes.get(node.item.parent_id) ?? null)
      : null;
    if (!parent || parent === node) return 0;
    return Math.min(depthOf(parent, seen) + 1, 2);
  };
  for (const node of nodes.values()) node.depth = depthOf(node, new Set());
  const live = new Map<string, number>();
  const subtree = (node: TreeNode, seen: Set<string>): number => {
    if (seen.has(node.item.id)) return 0;
    seen.add(node.item.id);
    let count = 0;
    for (const child of node.children) {
      if (childLive(child.item)) count += 1;
      count += subtree(child, seen);
    }
    return count;
  };
  for (const node of nodes.values()) live.set(node.item.id, subtree(node, new Set()));
  const asking = (node: TreeNode, seen: Set<string>): number => {
    if (seen.has(node.item.id)) return 0;
    seen.add(node.item.id);
    let count = 0;
    for (const child of node.children) count += (child.item.activity.state === "waiting_for_user" ? 1 : 0) + asking(child, seen);
    return count;
  };
  for (const node of nodes.values()) {
    node.running = live.get(node.item.id) ?? 0;
    node.waiting = asking(node, new Set());
    node.open = kids.expanded.has(node.item.id)
      ? true
      : kids.collapsed.has(node.item.id)
        ? false
        : node.running > 0 || node.children.length === 0;
  }
  const top: TreeNode[] = [];
  for (const item of pool) {
    const node = nodes.get(item.id)!;
    if (rootSet.has(node)) top.push(node);
  }
  return top;
}

function reaches(from: TreeNode, id: string, nodes: Map<string, TreeNode>): boolean {
  let current: TreeNode | null | undefined = from;
  const seen = new Set<string>();
  while (current && !seen.has(current.item.id)) {
    if (current.item.id === id) return true;
    seen.add(current.item.id);
    const parentId: string | null = current.item.parent_id;
    current =
      parentId && parentId !== current.item.id ? (nodes.get(parentId) ?? null) : null;
  }
  return false;
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
  items: TreeNode[],
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

/** Заголовок детей: сколько работает и сколько ждёт владельца, раздельно. */
export function kidsTitle(node: Pick<TreeNode, "children" | "running" | "waiting">): string {
  const working = node.running - node.waiting;
  return [`Делегированные · ${node.children.length}`, working > 0 ? `работает ${working}` : "", node.waiting > 0 ? `ждёт ${node.waiting}` : ""].filter(Boolean).join(" · ");
}
