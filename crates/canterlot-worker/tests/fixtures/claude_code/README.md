# Hand-built fixtures for the Claude Code stream parser (Task 2)

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
