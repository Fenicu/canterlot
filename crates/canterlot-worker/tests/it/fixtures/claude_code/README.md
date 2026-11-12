# Fixtures for the Claude Code adapter tests (Tasks 2, 6)

Recorded evidence lives in `docs/stage0/evidence/claude-code/` and needs no
listing here. The files in the first list are **constructed, confirm in live
smoke**: each follows a recorded frame shape, the sources are given per file.
The second list was captured from the live smoke and replaces constructed
files where the live CLI reached the behaviour.

- `subagent.jsonl` — constructed before the live capture (`subagent_live.jsonl`
  below shows the real frames); kept for a CLI that streams the subagent, so its
  text must not double. Shape sources:
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
- `local_command.jsonl` — `/context` in «Без инструкций» through a symlinked
  workdir. The output of a local command is a single `assistant` frame with
  `local_command_source`, model `<synthetic>` and no stream frames; the
  adapter turns its text into a text block. The output and `context_usage`
  are trimmed to the memory section (only the repository `CLAUDE.md`: the
  owner's `~/CLAUDE.md` stays out although the CLI walks the physical path).
- `read_image.jsonl` — `Read` of a 1x1 PNG in the workdir (replaces the
  constructed file). Confirms the open question of plan 2a: the
  `tool_result` content is a list with one
  `{"type":"image","source":{"type":"base64","media_type":"image/png","data":…}}`
  block; in mode `ask` the CLI allows `Read` inside the workdir itself, so
  the tool block carries `approved_by: "cli_policy"`.
- `subagent_live.jsonl` — `Agent` (`Explore`, haiku) through `claude -p`, bypass
  mode, no MCP servers, with `Bash` and `Read` inside the subagent, then a `Read`
  of a missing file by the main agent. Replaces the assumption of
  `subagent.jsonl`: a subagent has no `stream_event` frames at all. Its
  `tool_use`, `text` and `thinking` arrive only as whole `assistant` frames with
  `parent_tool_use_id` set to the `Agent` call, interleaved with the stream of
  the main agent, and its `tool_result` as `user` frames with the same parent.
  `is_error: true` comes on the `tool_result` block of the failed `Read`.
  Scrubbed: paths, ids, signatures, `rate_limit_event`, and `system` frames down
  to what the parser reads.
- `todo_write.jsonl` — `TodoWrite` through `claude -p` (not the live smoke),
  bypass mode, no MCP servers, with `CLAUDE_CODE_ENABLE_TODO_TOOLS=true` and
  `CLAUDE_CODE_ENABLE_TASKS=false`. The same prompt without those variables got
  no todo tool on `claude-haiku-5-5`: CLI 2.1.311 gates `TodoWrite` and `Task*`
  to models up to `claude-*-4-*`, and `system/init` `tools` lists neither.
  `TodoWrite` is deferred, so `ToolSearch` comes first. Scrubbed: ids, hook,
  thinking and rate limit frames, and `system/init` down to what the parser
  reads. The list arrives whole in the `user` frame as
  `tool_use_result.newTodos`, the same frame for both lanes.
- `background.jsonl`, `background_race.jsonl` — background work through
  `claude -p` (not the live smoke), bypass mode, no MCP servers, `--tools
  Agent,Bash`. The first starts a background `Agent` and a background `Bash`,
  sends a second message 3 s after the `result` and waits for both completions
  and their wake turns. The second writes a message at the instant the
  `task_notification` of the agent arrives: the wake turn starts, its `result`
  (`origin.kind: "task-notification"`, no `user_message_uuids`) comes first, then
  the replay, the answer and the `result` of the message. `in` rows are what was
  written to stdin. Scrubbed: paths, thinking signatures, `prompt` of
  `task_started`, `rate_limit_event`, and `system/init` down to what the parser
  reads.
- `prompt_suggestion.jsonl` — live capture (CLI 2.1.322, haiku, `--prompt-suggestions
  true`, no tools, empty strict MCP config). Message text, usage and ids are
  trimmed. Covers: the `prompt_suggestion` frame after the turn's `result`
  (`suggestion`, `uuid`, `session_id`), and the `control_response` to
  `set_prompt_suggestions_paused` (`{"paused": true}`), which the CLI accepts.
  The pause itself proved unreliable live (it took effect a turn late), so the
  adapter does not send it.
