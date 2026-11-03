import { describe, expect, it } from "vitest";
import {
  buildSections,
  tabCounts,
  type Group,
} from "#lib/screens/sessions/grouping.js";
import type { ListRow } from "#lib/stores/list.svelte.js";

function row(patch: Partial<ListRow> & { id: string; title: string }): ListRow {
  return {
    title_source: "user",
    color: null,
    pinned: false,
    archived: false,
    groups: [],
    group_pins: [],
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
  { id: "review", name: "На проверку", position: 1 },
  { id: "books", name: "bookshelf", position: 2 },
];

describe("sessions/grouping", () => {
  it("sections_put_pinned_first_and_show_each_session_once_in_the_all_view", () => {
    const items = [
      row({
        id: "p1",
        title: "Восстановление потока",
        pinned: true,
        groups: ["canterlot"],
      }),
      row({ id: "p2", title: "Очередь сообщений", pinned: true }),
      row({ id: "c1", title: "Граница", groups: ["canterlot"] }),
      row({ id: "c2", title: "Воркер", groups: ["canterlot"] }),
      row({ id: "c3", title: "Адаптер", groups: ["canterlot", "review"] }),
      row({ id: "r1", title: "Доставка", groups: ["review"] }),
      row({ id: "b1", title: "Поиск", groups: ["books"] }),
      row({ id: "b2", title: "Каталог", groups: ["books"] }),
      row({ id: "b3", title: "Обложка", groups: ["books"] }),
      row({ id: "b4", title: "Глава", groups: ["books"] }),
    ];
    const sections = buildSections(items, groups, { kind: "all" }, new Set());
    expect(sections.map((section) => section.items.length)).toEqual([
      2, 3, 1, 4,
    ]);
    expect(sections.reduce((sum, section) => sum + section.count, 0)).toBe(10);
    expect(
      new Set(
        sections.flatMap((section) => section.items.map((node) => node.item.id)),
      ).size,
    ).toBe(10);
    expect(sections[0]?.items.map((node) => node.item.id)).toEqual(["p1", "p2"]);
    expect(sections[1]?.title).toBe("canterlot");
  });

  it("group_tab_lists_all_members_including_pinned", () => {
    const items = [
      row({ id: "p1", title: "Закреп", pinned: true, groups: ["canterlot"] }),
      row({ id: "c1", title: "Обычная", groups: ["canterlot"] }),
      row({ id: "other", title: "Чужая", groups: ["books"] }),
    ];
    const [section] = buildSections(
      items,
      groups,
      { kind: "group", id: "canterlot" },
      new Set(),
    );
    expect(section?.items.map((node) => node.item.id)).toEqual(["p1", "c1"]);
  });

  it("group_tab_puts_group_pins_first_newest_and_keeps_global_pins_in_the_rest", () => {
    const items = [
      row({
        id: "global",
        title: "Глобальный закреп",
        pinned: true,
        groups: ["canterlot"],
      }),
      row({ id: "plain", title: "Обычная", groups: ["canterlot"] }),
      row({
        id: "older",
        title: "Раньше закреплена",
        groups: ["canterlot", "books"],
        group_pins: [
          { group_id: "canterlot", pinned_at: "2026-10-28T10:00:00.000Z" },
          { group_id: "books", pinned_at: "2026-10-28T11:30:00.000Z" },
        ],
      }),
      row({
        id: "newer",
        title: "Позже закреплена",
        groups: ["canterlot"],
        group_pins: [
          { group_id: "canterlot", pinned_at: "2026-10-28T12:00:00.000Z" },
        ],
      }),
    ];
    const sections = buildSections(
      items,
      groups,
      { kind: "group", id: "canterlot" },
      new Set(),
    );
    expect(sections.map((section) => section.title)).toEqual([
      "Закреплённые",
      "canterlot",
    ]);
    expect(sections[0]?.items.map((node) => node.item.id)).toEqual([
      "newer",
      "older",
    ]);
    expect(sections[0]?.count).toBe(2);
    expect(sections[1]?.items.map((node) => node.item.id)).toEqual([
      "global",
      "plain",
    ]);

    // «Все» и чужая группа знают только свои закрепления.
    const all = buildSections(items, groups, { kind: "all" }, new Set());
    expect(all[0]?.title).toBe("Закреплённые");
    expect(all[0]?.items.map((node) => node.item.id)).toEqual(["global"]);
    const books = buildSections(
      items,
      groups,
      { kind: "group", id: "books" },
      new Set(),
    );
    expect(books[0]?.title).toBe("Закреплённые");
    expect(books[0]?.items.map((node) => node.item.id)).toEqual(["older"]);
  });

  it("group_with_every_member_pinned_still_shows_the_pin_heading", () => {
    const items = [
      row({
        id: "only",
        title: "Одна",
        groups: ["books"],
        group_pins: [
          { group_id: "books", pinned_at: "2026-10-28T12:00:00.000Z" },
        ],
      }),
    ];
    const sections = buildSections(
      items,
      groups,
      { kind: "group", id: "books" },
      new Set(),
    );
    expect(sections).toHaveLength(1);
    expect(sections[0]?.title).toBe("Закреплённые");
  });

  it("counts_come_from_the_snapshot_and_events_not_the_visible_slice", () => {
    const tabs = tabCounts(
      {
        all: 10,
        pinned: 2,
        archived: 4,
        groups: [{ id: "canterlot", count: 3 }],
      },
      [{ id: "canterlot", name: "canterlot" }],
    );
    expect(tabs.map((tab) => tab.count)).toEqual([10, 2, 3, 4]);
    expect(tabs.map((tab) => tab.label)).toEqual([
      "Все",
      "Закреплённые",
      "canterlot",
      "Архив",
    ]);
    const visible = buildSections(
      [row({ id: "only", title: "одна" })],
      groups,
      { kind: "all" },
      new Set(),
    );
    expect(visible[0]?.count).toBe(1);
    expect(tabs[0]?.count).toBe(10);
  });
});
