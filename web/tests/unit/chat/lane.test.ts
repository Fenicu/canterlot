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
    expect(model.breadcrumb).toEqual(["Оркестратор", "Explore"]);
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
});
