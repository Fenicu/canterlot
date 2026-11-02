import { afterEach, describe, expect, it } from "vitest";
import { readTodoOpen, writeTodoOpen } from "#lib/screens/chat/todoOpen.js";

describe("chat/todoOpen", () => {
  afterEach(() => localStorage.clear());

  it("defaults_to_no_choice_until_the_owner_toggles_the_panel", () => {
    expect(readTodoOpen()).toBeNull();
    writeTodoOpen(true);
    expect(readTodoOpen()).toBe(true);
    writeTodoOpen(false);
    expect(readTodoOpen()).toBe(false);
  });

  it("broken_storage_reads_as_no_choice", () => {
    localStorage.setItem("canterlot.todo-open", "not json");
    expect(readTodoOpen()).toBeNull();
  });
});
