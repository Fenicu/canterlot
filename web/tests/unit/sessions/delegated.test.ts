import { describe, expect, it } from "vitest";
import { buildSections, type Group } from "#lib/screens/sessions/grouping.js";
import type { ListRow } from "#lib/stores/list.svelte.js";

function row(patch: Partial<ListRow> & { id: string; title: string }): ListRow {
  return {
    title_source: "user",
    color: null,
    pinned: false,
    archived: false,
    groups: [],
    parent_id: null,
    spawn_depth: 0,
    machine: { id: "m", name: "workstation" },
    harness: { id: "claude", title: "Claude Code", provider: "claude" },
    workdir: "/work",
    created_at: "2026-10-28T12:00:00.000Z",
    model: "opus",
    effort: "medium",
    permission_mode: "default",
    activity: { state: "idle" },
    unread_count: 0,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
    crunch: { enabled: false },
    last_activity_at: null,
    ...patch,
  };
}

const groups: Group[] = [
  { id: "canterlot", name: "canterlot", position: 0 },
  { id: "books", name: "bookshelf", position: 1 },
];

describe("sessions/delegated-tree", () => {
  it("child_nests_under_its_parent_and_leaves_counts_and_groups_alone", () => {
    const items = [
      row({ id: "parent", title: "Оркестратор", groups: ["canterlot"] }),
      row({
        id: "child",
        title: "Миграция",
        parent_id: "parent",
        spawn_depth: 1,
        activity: { state: "working", evidence: { phase: "generating" } },
      }),
      row({ id: "plain", title: "Обычная" }),
    ];
    const sections = buildSections(items, groups, { kind: "all" }, new Set());
    const flat = sections.flatMap((section) => section.items);
    // Ребёнок не образует своей группы и не входит в счёт секции.
    expect(sections.map((section) => section.title)).toEqual([
      "canterlot",
      "Без группы",
    ]);
    expect(sections.map((section) => section.count)).toEqual([1, 1]);
    const parent = flat.find((node) => node.item.id === "parent");
    expect(parent?.children.map((node) => node.item.id)).toEqual(["child"]);
    expect(parent?.children[0]?.depth).toBe(1);
    expect(parent?.open).toBe(true);
    expect(parent?.running).toBe(1);
    // Ребёнок не занимает места в списке верхнего уровня.
    expect(flat.map((node) => node.item.id).sort()).toEqual(["parent", "plain"]);
  });

  it("closed_by_default_when_every_child_is_finished_and_choice_persists", () => {
    const items = [
      row({ id: "parent", title: "Оркестратор" }),
      row({
        id: "child",
        title: "Миграция",
        parent_id: "parent",
        spawn_depth: 1,
        activity: { state: "stopped", evidence: {} },
      }),
    ];
    const closed = buildSections(items, [], { kind: "all" }, new Set());
    expect(closed[0]?.items[0]?.open).toBe(false);
    const opened = buildSections(items, [], { kind: "all" }, new Set(), {
      collapsed: new Set(),
      expanded: new Set(["parent"]),
    });
    expect(opened[0]?.items[0]?.open).toBe(true);
  });

  it("orphan_of_a_filtered_parent_stays_a_plain_row", () => {
    const items = [
      row({
        id: "child",
        title: "Миграция",
        parent_id: "missing",
        spawn_depth: 1,
      }),
    ];
    const sections = buildSections(items, [], { kind: "all" }, new Set());
    expect(sections).toHaveLength(1);
    expect(sections[0]?.items.map((node) => node.item.id)).toEqual(["child"]);
    expect(sections[0]?.items[0]?.depth).toBe(0);
  });

  it("depth_stops_growing_at_two", () => {
    const items = [
      row({ id: "a", title: "Корень" }),
      row({ id: "b", title: "Дети", parent_id: "a", spawn_depth: 1 }),
      row({ id: "c", title: "Внуки", parent_id: "b", spawn_depth: 2 }),
      row({ id: "d", title: "Правнуки", parent_id: "c", spawn_depth: 3 }),
    ];
    const sections = buildSections(items, [], { kind: "all" }, new Set());
    const a = sections[0]?.items[0];
    const b = a?.children[0];
    const c = b?.children[0];
    const d = c?.children[0];
    expect([a?.depth, b?.depth, c?.depth, d?.depth]).toEqual([0, 1, 2, 2]);
  });

  it("archived_children_follow_the_archive_tab_only", () => {
    const items = [
      row({ id: "parent", title: "Оркестратор" }),
      row({
        id: "child",
        title: "Миграция",
        parent_id: "parent",
        spawn_depth: 1,
        archived: true,
      }),
    ];
    const all = buildSections(items, [], { kind: "all" }, new Set());
    expect(all.flatMap((section) => section.items).map((node) => node.item.id)).toEqual([
      "parent",
    ]);
    const archive = buildSections(items, [], { kind: "archive" }, new Set());
    // Родитель не в архиве: ребёнок виден обычной строкой.
    expect(archive.flatMap((section) => section.items).map((node) => node.item.id)).toEqual([
      "child",
    ]);
  });
});
