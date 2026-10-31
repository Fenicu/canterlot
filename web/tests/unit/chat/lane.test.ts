import { describe, expect, test } from "vitest";
import { laneModel } from "#lib/screens/chat/lane.js";
import { BlockModel } from "#lib/stores/blocks.js";

describe("laneModel", () => {
  test("lane_model_builds_breadcrumb_task_and_status", () => {
    const root = new BlockModel("agent-tool-1", "tool_call");
    root.toolName = "Agent";
    root.agent = "Explore";
    root.model = "haiku";
    root.summary = "Поиск дедупликации";
    root.state = "streaming";

    const child = new BlockModel("child-1", "tool_call");
    child.parentId = "agent-tool-1";
    child.toolName = "Read";
    child.summary = "src/streams/replay.rs";
    child.state = "final";

    const blocks = [root, child];

    // While streaming
    let model = laneModel(blocks, "agent-tool-1");
    expect(model.title).toBe("Поиск дедупликации");
    expect(model.subtitle).toBe("Субагент · Explore · haiku · workstation");
    expect(model.breadcrumb).toEqual(["Агент", "Explore"]);
    expect(model.task).toBe("Поиск дедупликации");
    expect(model.status).toBe("Работает параллельно с основным агентом");
    expect(model.running).toBe(true);

    // When finished
    root.state = "final";
    model = laneModel(blocks, "agent-tool-1");
    expect(model.status).toBe("Готово");
    expect(model.running).toBe(false);

    // When interrupted
    root.state = "incomplete";
    model = laneModel(blocks, "agent-tool-1");
    expect(model.status).toBe("Прервано");
    expect(model.running).toBe(false);
  });

  test("lane_of_a_background_subagent_follows_its_task", () => {
    const root = new BlockModel("agent-tool-1", "tool_call");
    root.toolName = "Agent";
    root.state = "final";
    const launch = new BlockModel("launch-1", "tool_result");
    launch.parentId = "agent-tool-1";
    launch.summary = "async_launched";
    launch.state = "final";
    const blocks = [root, launch];
    const fact = (state: string) => ({ state, kind: "agent", endedAt: null });

    const running = laneModel(blocks, "agent-tool-1", { result: launch, working: false, background: fact("running") });
    expect(running.running).toBe(true);
    const done = laneModel(blocks, "agent-tool-1", { result: launch, working: false, background: fact("completed") });
    expect([done.running, done.status]).toEqual([false, "Готово"]);
    const failed = laneModel(blocks, "agent-tool-1", { result: launch, working: false, background: fact("failed") });
    expect(failed.status).toBe("Ошибка");
    const stopped = laneModel(blocks, "agent-tool-1", { result: launch, working: false, background: fact("stopped") });
    expect(stopped.status).toBe("Прервано");
  });
});
