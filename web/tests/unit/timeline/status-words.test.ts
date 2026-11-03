import { describe, expect, it } from "vitest";
import { BlockModel } from "#lib/stores/blocks.js";
import { nodeStatus, STATUS_TONE, STATUS_WORD, subagentCopy, toolError } from "#lib/timeline/present.js";

function call(state: BlockModel["state"]): BlockModel {
  const block = new BlockModel("call", "tool_call");
  block.toolName = "Bash";
  block.state = state;
  block.createdAt = 1000;
  block.finishedAt = 2000;
  return block;
}

describe("status_words", () => {
  it("interrupted_tool_is_not_an_error", () => {
    expect(toolError(call("incomplete"), null)).toBe(false);
    const failed = new BlockModel("result", "tool_result");
    failed.summary = "error";
    expect(toolError(call("final"), failed)).toBe(true);
  });

  it("panel_and_feed_use_one_word_and_tone_for_each_end", () => {
    const agent = call("final");
    agent.toolName = "Agent";
    const feed = subagentCopy(agent, [], { result: null, working: false });
    expect(feed.state).toBe("stopped");
    expect(feed.detail.startsWith(STATUS_WORD.stopped)).toBe(true);
    for (const state of ["incomplete", "stopped", "aborted", "stale"]) expect(nodeStatus(state)).toBe("stopped");
    expect(STATUS_WORD.stopped).toBe("Прервано");
    expect(STATUS_TONE.stopped).toBe("warning");
    expect(STATUS_TONE.error).toBe("danger");
    expect(STATUS_TONE.done).toBe("muted");
    expect(nodeStatus("final")).toBe("done");
    expect(nodeStatus("failed")).toBe("error");
  });
});
