import { describe, expect, it } from "vitest";
import { todoView, TODO_STATUS_LABEL } from "#lib/screens/chat/todo.js";
import type { TodoList, TodoStatus } from "#lib/stores/todos.svelte.js";

function listOf(statuses: TodoStatus[], omitted = 0): TodoList {
  return {
    lane_block_id: null,
    revision: 1,
    omitted,
    items: statuses.map((status, i) => ({ text: `пункт ${i + 1}`, status })),
  };
}

describe("screens/chat/todo", () => {
  it("statuses_are_words", () => {
    expect(TODO_STATUS_LABEL).toEqual({
      pending: "ожидает",
      in_progress: "в работе",
      completed: "готово",
    });
  });

  it("progress_counts_finished_items_and_names_the_one_in_work", () => {
    const view = todoView(listOf(["completed", "in_progress", "pending"]));
    expect(view).toMatchObject({ done: 1, total: 3, allDone: false });
    expect(view.current?.text).toBe("пункт 2");
    expect(view.index).toBe(1);
    expect(view.stages.map((stage) => stage.label)).toEqual([
      "пункт 1",
      "пункт 2",
      "пункт 3",
      "Готово",
    ]);
  });

  it("order_of_work_does_not_matter_for_progress", () => {
    const view = todoView(listOf(["pending", "completed", "in_progress"]));
    expect(view.done).toBe(1);
    expect(view.current?.text).toBe("пункт 3");
  });

  it("without_work_in_progress_next_pending_item_is_current", () => {
    const view = todoView(listOf(["completed", "pending", "pending"]));
    expect(view.current?.text).toBe("пункт 2");
  });

  it("finished_list_lands_on_the_last_point", () => {
    const view = todoView(listOf(["completed", "completed"]));
    expect(view).toMatchObject({ done: 2, total: 2, allDone: true, index: 2 });
    expect(view.current).toBeNull();
  });

  it("long_list_shows_a_real_window_of_points_around_the_progress", () => {
    const statuses: TodoStatus[] = [
      ...Array<TodoStatus>(12).fill("completed"),
      "in_progress",
      ...Array<TodoStatus>(7).fill("pending"),
    ];
    const view = todoView(listOf(statuses));
    expect(view.stages).toHaveLength(5);
    expect(view.stages[view.index]?.label).toBe("пункт 13");
    expect(new Set(view.stages.map((stage) => stage.id)).size).toBe(5);
  });

  it("window_reaches_the_finish_point_only_at_the_end", () => {
    const statuses = Array<TodoStatus>(20).fill("completed");
    statuses[19] = "in_progress";
    const nearEnd = todoView(listOf(statuses));
    expect(nearEnd.stages.at(-1)?.label).toBe("Готово");
    expect(nearEnd.index).toBeLessThan(nearEnd.stages.length - 1);
    const finished = todoView(listOf(Array<TodoStatus>(20).fill("completed")));
    expect(finished.index).toBe(finished.stages.length - 1);
  });

  it("dropped_items_count_towards_the_total", () => {
    expect(todoView(listOf(["completed", "pending"], 40)).total).toBe(42);
  });
});
