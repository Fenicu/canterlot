import { beforeEach, describe, expect, it } from "vitest";
import { clearOwnerPreferences } from "#lib/pwa/signout.js";
import {
  LIST_TAB_KEY,
  readTab,
  resolveTab,
  writeTab,
} from "#lib/screens/sessions/listTab.js";

beforeEach(() => {
  localStorage.clear();
});

describe("sessions/list tab", () => {
  it("stores_the_choice_and_reads_it_back", () => {
    writeTab({ kind: "group", id: "books" });
    expect(localStorage.getItem(LIST_TAB_KEY)).toBe(
      JSON.stringify({ kind: "group", id: "books" }),
    );
    expect(readTab()).toEqual({ kind: "group", id: "books" });
  });

  it("without_a_stored_value_starts_on_all", () => {
    expect(readTab()).toEqual({ kind: "all" });
  });

  it("an_invalid_value_falls_back_to_all_and_is_overwritten", () => {
    localStorage.setItem(LIST_TAB_KEY, "{не json");
    expect(readTab()).toEqual({ kind: "all" });
    expect(localStorage.getItem(LIST_TAB_KEY)).toBe(
      JSON.stringify({ kind: "all" }),
    );

    localStorage.setItem(LIST_TAB_KEY, JSON.stringify({ kind: "group" }));
    expect(readTab()).toEqual({ kind: "all" });
    expect(localStorage.getItem(LIST_TAB_KEY)).toBe(
      JSON.stringify({ kind: "all" }),
    );
  });

  it("a_stored_group_that_no_longer_exists_falls_back_to_all_and_is_overwritten", () => {
    localStorage.setItem(
      LIST_TAB_KEY,
      JSON.stringify({ kind: "group", id: "gone" }),
    );
    const groups = [{ id: "books" }, { id: "review" }];
    expect(resolveTab({ kind: "group", id: "gone" }, groups)).toEqual({
      kind: "all",
    });
    expect(localStorage.getItem(LIST_TAB_KEY)).toBe(
      JSON.stringify({ kind: "all" }),
    );
  });

  it("a_stored_group_that_exists_stays_selected", () => {
    const tab = { kind: "group" as const, id: "books" };
    expect(resolveTab(tab, [{ id: "books" }])).toBe(tab);
    expect(localStorage.getItem(LIST_TAB_KEY)).toBeNull();
  });

  it("purging_the_device_forgets_the_choice", () => {
    localStorage.setItem(LIST_TAB_KEY, JSON.stringify({ kind: "archive" }));
    localStorage.setItem("canterlot.theme", "dark");
    clearOwnerPreferences(localStorage);
    expect(localStorage.getItem(LIST_TAB_KEY)).toBeNull();
  });
});
