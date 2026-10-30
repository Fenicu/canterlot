# Hand-built fixtures for the Claude Code adapter tests (Tasks 2, 6)

Recorded evidence lives in `docs/stage0/evidence/claude-code/` and needs no
listing here. The files below are **constructed, confirm in live smoke**.
Each follows a recorded frame shape; the sources are given per file.

- `subagent.jsonl` — constructed, confirm in live smoke. Shape sources:
  the recorded `Task` entry in `system/init` `tools` (`tools.jsonl:14`),
  the Anthropic `Task` tool input (`description`, `subagent_type`, `prompt`),
  and stream frames echoing the subagent turn's `parent_tool_use_id`
  (no recorded frame carries a non-null one — stage-0 has only background
  `task_*` events). Covers: a `Task` tool call with `subagent_type`,
  a subagent turn (`message_start`, text block, final `assistant`) tagged
  with `parent_tool_use_id`, and a `result` whose `modelUsage` spans the
  orchestrator model and the subagent-only model.
- `context_overflow.jsonl` — constructed, confirm in live smoke. Shape
  sources: the recorded error `result` frame (`service_failure.jsonl:82`,
  `is_error`, `api_error_status`, `terminal_reason`) and the Anthropic API
  "prompt is too long" error text. Covers: an error result whose text says
  the prompt is too long, which the parser must classify as
  `ContextExhausted` instead of a plain error.
- `read_image.jsonl` — constructed, confirm in live smoke. Shape sources:
  the recorded `Read` tool call and `tool_result` user frame
  (`tools.jsonl:19-57`: `tool_use` streamed through `input_json_delta`,
  `tool_result` with `tool_use_id`, `is_error`) and the Anthropic
  tool-result image block (`{"type":"image","source":{"type":"base64",
  "media_type","data"}}`). No recorded run has the CLI read an image, so
  the exact result shape is open (plan 2a, Open Questions). Covers: a
  `Read` of `/tmp/canterlot-fixture/shot.png` (the file does not exist)
  whose `tool_result` content is a single image block with a 4x3 PNG; the
  adapter must publish those bytes, not the file.
- `limit_rejected.jsonl` — constructed, confirm in live smoke (stage 0 never
  hit a limit). Shape source: the recorded `rate_limit_event`
  (`resume.jsonl:25`) with the top-level `status` and the `seven_day`
  window switched to `"rejected"` and `utilization` 1.0. Covers: a rejected
  window becomes `LimitFact::Reached` with that window's `resetsAt`; the
  recorded `overageStatus: "rejected"` alone is not a limit.
- `usage_limit_429.jsonl` — constructed, confirm when a limit is actually
  reached. Shape sources: pilot.md §15 q5 and the research
  (`.superpowers/peer/gemini-r1-stage2-research.md`, §1 q5: `system/api_retry`
  with `error_status: 429`, an error `assistant` frame with
  `api_error: "usage_limit_reached"` and `api_error_params.rate_limit_info`,
  a `result` with `is_error` and `api_error_status: 429`), the recorded error
  `assistant` frame (`service_failure.jsonl:19`) and `rate_limit_event`
  (`resume.jsonl:25`). Covers: the reset time of `Reached` comes from the
  error's `rate_limit_info` (it differs from the earlier event on purpose).

## Captured in the live smoke (CLI 2.1.311, haiku, effort low)

Recorded from the real CLI by the live smoke (`claude_code_e2e.rs`,
`live_smoke`) and scrubbed: hook frames and the owner's MCP servers, plugins,
skills and commands are dropped from `system/init`, thinking signatures are
emptied, the temp workdir becomes `/tmp/canterlot-smoke` and `$HOME`
becomes `/home/owner`. Same wrapper as stage 0: `dir == "in"` is what the
adapter wrote, `"out"` is CLI stdout, `"note"` carries MCP traffic
(`mcp_trace`) or a marker (`signal`: where the smoke sent SIGINT).

- `interrupt_streaming.jsonl` — SIGINT while the model streams (the CLI
  moved a `sleep 60` chain to the background, so no tool was running): the
  `result` has `terminal_reason: "aborted_streaming"`, not `aborted_tools`;
  both acknowledge the interrupt.
