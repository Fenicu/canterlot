#![forbid(unsafe_code)]

//! Stream-JSON parser tests (plan Task 2, Step 1). Recorded fixtures come
//! from docs/stage0/evidence/claude-code/; constructed ones live in
//! tests/fixtures/claude_code/ (see its README.md).

use canterlot_worker::adapter::{
    AdapterEvent, BlockKind, Phase, TodoItem, TodoStatus,
    claude_code::parser::{Parsed, Parser},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "support/fixtures.rs"]
mod fixtures;

const COMMAND: &str = "22222222-2222-4222-8222-222222222222";

fn command_id() -> Uuid {
    Uuid::parse_str("f9ec973c-53f4-4369-9109-786f38fcb361").unwrap()
}

fn feed_all(parser: &mut Parser, lines: &[String]) -> Vec<Parsed> {
    let mut out = Vec::new();
    for line in lines {
        out.extend(parser.feed(line));
    }
    out
}

fn deltas(parsed: &[Parsed]) -> Vec<&Parsed> {
    parsed
        .iter()
        .filter(|p| matches!(p, Parsed::Event(AdapterEvent::BlockDelta { .. })))
        .collect()
}

fn finals(parsed: &[Parsed]) -> Vec<&AdapterEvent> {
    parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(event @ AdapterEvent::BlockFinal { .. }) => Some(event),
            _ => None,
        })
        .collect()
}

#[test]
fn start_stream_deltas_join_to_the_final_text() {
    let replay = fixtures::replay_evidence("start_stream");
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = feed_all(&mut parser, &replay.out);
    let text: String = deltas(&parsed)
        .iter()
        .map(|p| match p {
            Parsed::Event(AdapterEvent::BlockDelta { append, .. }) => append.as_str(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        text, "1 2 3 4 5 6 7 8 9 10 11 12 13 14 15",
        "deltas must join to the final text"
    );
    let finals = finals(&parsed);
    assert_eq!(finals.len(), 1, "one final text block");
    match &finals[0] {
        AdapterEvent::BlockFinal { text, .. } => assert_eq!(
            text, "1 2 3 4 5 6 7 8 9 10 11 12 13 14 15",
            "final text repeats the joined deltas"
        ),
        other => panic!("expected BlockFinal, got {other:?}"),
    }
    let phases: Vec<&Phase> = parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::Phase { phase, .. }) => Some(phase),
            _ => None,
        })
        .collect();
    assert!(
        phases
            .iter()
            .any(|phase| matches!(phase, Phase::Generating)),
        "message_start phases"
    );
    assert!(
        phases
            .iter()
            .any(|phase| matches!(phase, Phase::AwaitingInput)),
        "result phases back to input"
    );
}

#[test]
fn tool_input_stays_buffered_until_block_stop() {
    let replay = fixtures::replay_evidence("tools");
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let mut completes = 0;
    for line in &replay.out {
        let is_stop = line.contains("content_block_stop");
        for parsed in parser.feed(line) {
            if let Parsed::ToolCallComplete { .. } = parsed {
                assert!(is_stop, "a complete input must only surface at block stop");
                completes += 1;
            }
        }
    }
    assert_eq!(completes, 2, "two tool calls complete");
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = feed_all(&mut parser, &replay.out);
    let mut inputs: Vec<(&str, Value)> = parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::ToolCallComplete {
                tool_name, input, ..
            } => Some((tool_name.as_str(), input.clone())),
            _ => None,
        })
        .collect();
    inputs.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0].0, "Bash");
    assert_eq!(
        inputs[0].1,
        json!({"command": "ls -a", "description": "List all files in current directory"})
    );
    assert_eq!(inputs[1].0, "Read");
    assert_eq!(
        inputs[1].1,
        json!({"file_path": "/tmp/canterlot-stage0/claude-code/tools/does-not-exist.txt"})
    );
}

#[test]
fn invalid_tool_json_becomes_error_block_and_parsing_continues() {
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let frames = [
        r#"{"type":"stream_event","event":{"content_block":{"id":"toolu_broken","input":{},"name":"Read","type":"tool_use"},"index":0,"type":"content_block_start"},"parent_tool_use_id":null,"session_id":"s","type":"stream_event","uuid":"u1"}"#,
        r#"{"type":"stream_event","event":{"delta":{"partial_json":"{\"file_pa","type":"input_json_delta"},"index":0,"type":"content_block_delta"},"parent_tool_use_id":null,"session_id":"s","type":"stream_event","uuid":"u2"}"#,
        r#"{"type":"stream_event","event":{"index":0,"type":"content_block_stop"},"parent_tool_use_id":null,"session_id":"s","type":"stream_event","uuid":"u3"}"#,
    ];
    let mut parsed = feed_all(&mut parser, &frames.map(str::to_owned));
    assert!(
        parsed.iter().any(|p| matches!(
            p,
            Parsed::Event(AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            })
        )),
        "truncated tool input becomes an error block, got {parsed:?}"
    );
    assert!(
        !parsed
            .iter()
            .any(|p| matches!(p, Parsed::ToolCallComplete { .. })),
        "no complete input from broken JSON"
    );
    let continued = [
        r#"{"type":"stream_event","event":{"content_block":{"text":"","type":"text"},"index":1,"type":"content_block_start"},"parent_tool_use_id":null,"session_id":"s","type":"stream_event","uuid":"u4"}"#,
        r#"{"type":"stream_event","event":{"delta":{"text":"hi","type":"text_delta"},"index":1,"type":"content_block_delta"},"parent_tool_use_id":null,"session_id":"s","type":"stream_event","uuid":"u5"}"#,
        r#"{"type":"stream_event","event":{"index":1,"type":"content_block_stop"},"parent_tool_use_id":null,"session_id":"s","type":"stream_event","uuid":"u6"}"#,
    ];
    parsed = feed_all(&mut parser, &continued.map(str::to_owned));
    assert!(
        parsed.iter().any(|p| matches!(
            p,
            Parsed::Event(AdapterEvent::BlockDelta { append, .. }) if append == "hi"
        )),
        "the session continues after the error block, got {parsed:?}"
    );
}

#[test]
fn final_assistant_frame_does_not_duplicate_streamed_text() {
    let replay = fixtures::replay_evidence("tools");
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = feed_all(&mut parser, &replay.out);
    let text_deltas = deltas(&parsed).len();
    assert_eq!(
        text_deltas, 8,
        "only the streamed sentence arrives as deltas"
    );
    let mut started = std::collections::HashSet::new();
    let mut sentence_finals = 0;
    for p in &parsed {
        match p {
            Parsed::Event(AdapterEvent::BlockStarted { block_id, .. }) => {
                started.insert(*block_id);
            }
            Parsed::Event(AdapterEvent::BlockFinal { block_id, text, .. }) => {
                assert!(
                    started.contains(block_id),
                    "no final for a block the stream never started"
                );
                if text
                    == "The directory holds only `.git` and `hello.txt`, and `does-not-exist.txt` is missing as expected."
                {
                    sentence_finals += 1;
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        sentence_finals, 1,
        "one final text block, no assistant duplicate"
    );
}

#[test]
fn usage_is_per_turn_although_model_usage_is_cumulative() {
    let replay = fixtures::replay_evidence("multi_input");
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = feed_all(&mut parser, &replay.out);
    let mut per_turn = Vec::new();
    for p in &parsed {
        if let Parsed::Event(AdapterEvent::RunFinished { usage, .. }) = p {
            assert_eq!(usage.len(), 1, "one model per turn here");
            assert_eq!(usage[0].model, "claude-haiku-5-5");
            assert_eq!(usage[0].agent, "orchestrator");
            per_turn.push(usage[0].input_tokens);
        }
    }
    assert_eq!(
        per_turn,
        vec![2, 2, 2, 2, 2],
        "cumulative 2,4,6,8,10 splits per turn"
    );
}

#[test]
fn context_tokens_and_window_come_from_the_turn() {
    let replay = fixtures::replay_evidence("multi_input");
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = feed_all(&mut parser, &replay.out);
    let first = parsed
        .iter()
        .find_map(|p| match p {
            Parsed::Event(event @ AdapterEvent::RunFinished { .. }) => Some(event),
            _ => None,
        })
        .expect("a finished turn");
    match first {
        AdapterEvent::RunFinished {
            context_tokens,
            context_window_tokens,
            ..
        } => {
            assert_eq!(
                *context_tokens,
                2 + 12551 + 6178,
                "input + cache read + cache creation"
            );
            assert_eq!(*context_window_tokens, 1_000_000);
        }
        other => panic!("expected RunFinished, got {other:?}"),
    }
}

#[test]
fn subagent_frames_carry_name_model_and_parent_block() {
    let lines = fixtures::replay_fixture("subagent");
    let mut parser = Parser::new();
    parser.set_input_command(Uuid::parse_str(COMMAND).unwrap());
    let parsed = feed_all(&mut parser, &lines);
    let mut task_block = None;
    let mut sub_block = None;
    for p in &parsed {
        if let Parsed::Event(AdapterEvent::BlockStarted {
            block_id,
            kind,
            agent,
            parent,
            ..
        }) = p
        {
            match kind {
                BlockKind::ToolCall => {
                    assert_eq!(agent, "orchestrator");
                    assert_eq!(*parent, None);
                    task_block = Some(*block_id);
                }
                BlockKind::Text if agent == "Explore" => {
                    sub_block = Some((*block_id, parent));
                }
                _ => {}
            }
        }
    }
    let task_block = task_block.expect("task tool call block");
    let (sub_id, sub_parent) = sub_block.expect("subagent text block");
    assert_ne!(task_block, sub_id);
    assert_eq!(
        sub_parent,
        &Some(task_block),
        "subagent block links the Task call"
    );
    let mut models: Vec<String> = parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::BlockStarted { model, .. }) => Some(model.clone()),
            _ => None,
        })
        .collect();
    models.sort();
    models.dedup();
    assert!(
        models.contains(&"claude-haiku-5-5".to_owned()),
        "{models:?}"
    );
    assert!(models.contains(&"claude-opus-4-6".to_owned()), "{models:?}");
    let (tool_name, input) = parsed
        .iter()
        .find_map(|p| match p {
            Parsed::ToolCallComplete {
                tool_name, input, ..
            } => Some((tool_name.clone(), input.clone())),
            _ => None,
        })
        .expect("completed Task call");
    assert_eq!(tool_name, "Task");
    assert_eq!(input["subagent_type"], json!("Explore"));
}

struct Started {
    id: Uuid,
    kind: BlockKind,
    agent: String,
    parent: Option<Uuid>,
    tool_name: Option<String>,
}

fn started(parsed: &[Parsed]) -> Vec<Started> {
    parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::BlockStarted {
                block_id,
                kind,
                agent,
                parent,
                tool_name,
                ..
            }) => Some(Started {
                id: *block_id,
                kind: kind.clone(),
                agent: agent.clone(),
                parent: *parent,
                tool_name: tool_name.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[test]
fn subagent_tools_of_whole_frames_land_in_the_lane_of_its_call() {
    let lines = fixtures::replay_fixture("subagent_live");
    let mut parser = Parser::new();
    parser.set_input_command(Uuid::parse_str(COMMAND).unwrap());
    let parsed = feed_all(&mut parser, &lines);
    let blocks = started(&parsed);
    let call = blocks
        .iter()
        .find(|b| b.tool_name.as_deref() == Some("Agent"))
        .expect("Agent call");
    let tools: Vec<&Started> = blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::ToolCall) && b.agent == "Explore")
        .collect();
    let names: Vec<_> = tools.iter().map(|b| b.tool_name.as_deref()).collect();
    assert_eq!(names, [Some("Bash"), Some("Read")]);
    assert!(tools.iter().all(|b| b.parent == Some(call.id)));
    let summaries: Vec<_> = parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::BlockFinal {
                block_id,
                summary: Some(summary),
                ..
            }) if tools.iter().any(|tool| tool.id == *block_id) => Some(summary.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        summaries,
        [
            "Bash: find ./data -name note.txt",
            "Read: /tmp/canterlot-smoke/data/note.txt"
        ]
    );
    let results: Vec<&Started> = blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::ToolResult) && b.agent == "Explore")
        .collect();
    assert_eq!(results.len(), 2);
    for (result, tool) in results.iter().zip(&tools) {
        assert_eq!(result.parent, Some(tool.id));
    }
    assert!(
        blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ToolResult))
            .all(|b| b.parent.is_some()),
        "no result is left without its call"
    );
    let text = blocks
        .iter()
        .find(|b| matches!(b.kind, BlockKind::Text) && b.agent == "Explore")
        .expect("subagent text");
    assert_eq!(text.parent, Some(call.id));
    let completed = parsed
        .iter()
        .filter(|p| matches!(p, Parsed::ToolCallComplete { .. }))
        .count();
    assert_eq!(completed, 4, "Agent, Bash, Read and Read of the main agent");
}

#[test]
fn streamed_subagent_text_is_not_repeated_by_its_whole_frame() {
    let lines = fixtures::replay_fixture("subagent");
    let mut parser = Parser::new();
    let parsed = feed_all(&mut parser, &lines);
    let texts = started(&parsed)
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Text) && b.agent == "Explore")
        .count();
    assert_eq!(texts, 1);
}

#[test]
fn failed_tool_result_is_marked_in_its_summary() {
    let lines = fixtures::replay_fixture("subagent_live");
    let mut parser = Parser::new();
    let parsed = feed_all(&mut parser, &lines);
    let results: Vec<Uuid> = started(&parsed)
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::ToolResult))
        .map(|b| b.id)
        .collect();
    let marks: Vec<Option<&str>> = parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::BlockFinal {
                block_id, summary, ..
            }) if results.contains(block_id) => Some(summary.as_deref()),
            _ => None,
        })
        .collect();
    assert_eq!(marks, [None, None, None, Some("error")]);
}

#[test]
fn unknown_and_non_json_lines_never_end_the_stream() {
    let replay = fixtures::replay_evidence("resume");
    let mut lifecycles = 0;
    let mut parser = Parser::new();
    for line in &replay.out {
        if line.contains("command_lifecycle") {
            lifecycles += 1;
            assert!(
                matches!(parser.feed(line), single if single.len() == 1 && matches!(single[0], Parsed::Ignored)),
                "lifecycle frames are ignored"
            );
        }
    }
    assert!(lifecycles > 0, "resume carries lifecycle frames");
    assert!(
        matches!(
            parser.feed("not json")[..],
            [Parsed::Event(AdapterEvent::Diagnostic {
                kind: "unparsed_line",
                ..
            })]
        ),
        "non-JSON lines become an unparsed_line diagnostic"
    );
    assert!(
        matches!(
            parser.feed(r#"{"analyticsDisabled": true}"#)[..],
            [Parsed::Ignored]
        ),
        "valid JSON without a frame type is ignored"
    );
    let continued = parser.feed(
        r#"{"type":"stream_event","event":{"content_block":{"text":"","type":"text"},"index":0,"type":"content_block_start"},"type":"stream_event"}"#,
    );
    assert!(
        continued
            .iter()
            .any(|p| matches!(p, Parsed::Event(AdapterEvent::BlockStarted { .. }))),
        "block starts are accepted, got {continued:?}"
    );
    let continued = parser.feed(
        r#"{"type":"stream_event","event":{"delta":{"text":"ok","type":"text_delta"},"index":0,"type":"content_block_delta"},"type":"stream_event"}"#,
    );
    assert!(
        continued.iter().any(|p| matches!(
            p,
            Parsed::Event(AdapterEvent::BlockDelta { append, .. }) if append == "ok"
        )),
        "parsing continues, got {continued:?}"
    );
}

#[test]
fn rate_limit_event_is_passed_through_raw() {
    let replay = fixtures::replay_evidence("resume");
    let line = replay
        .out
        .iter()
        .find(|line| line.contains("rate_limit_event"))
        .expect("rate limit frame");
    let mut parser = Parser::new();
    let parsed = parser.feed(line);
    assert_eq!(parsed.len(), 1);
    match &parsed[0] {
        Parsed::RateLimit(info) => {
            assert_eq!(
                info["unifiedWindows"]["five_hour"]["utilization"],
                json!(0.5)
            );
            assert_eq!(
                info["unifiedWindows"]["seven_day"]["utilization"],
                json!(0.81)
            );
            assert_eq!(
                info["unifiedWindows"]["five_hour"]["resetsAt"],
                json!(1792919400)
            );
            assert_eq!(
                info["unifiedWindows"]["seven_day"]["resetsAt"],
                json!(1793062800)
            );
        }
        other => panic!("expected raw rate limit info, got {other:?}"),
    }
}

#[test]
fn debug_output_contains_no_text() {
    let block = Uuid::now_v7();
    let parsed = Parsed::ToolCallComplete {
        block_id: block,
        tool_use_id: "toolu_secret".into(),
        tool_name: "Bash".into(),
        input: json!({"command": "rm -rf /", "description": "delete everything"}),
    };
    let rendered = format!("{parsed:?}");
    assert!(!rendered.contains("rm -rf"), "{rendered}");
    assert!(!rendered.contains("delete everything"), "{rendered}");
    assert!(rendered.contains("toolu_secret"), "{rendered}");
    assert!(rendered.contains(&block.to_string()), "{rendered}");
    let event = Parsed::Event(AdapterEvent::BlockFinal {
        block_id: block,
        text: "secret-plans".into(),
        summary: None,
    });
    let rendered = format!("{event:?}");
    assert!(!rendered.contains("secret-plans"), "{rendered}");
    let mut parser = Parser::new();
    parser.feed(
        r#"{"type":"stream_event","event":{"content_block":{"text":"","type":"text"},"index":0,"type":"content_block_start"},"type":"stream_event"}"#,
    );
    parser.feed(
        r#"{"type":"stream_event","event":{"delta":{"text":"secret-plans","type":"text_delta"},"index":0,"type":"content_block_delta"},"type":"stream_event"}"#,
    );
    let rendered = format!("{parser:?}");
    assert!(!rendered.contains("secret-plans"), "{rendered}");
}

fn tool_call_frames(command: &str) -> Vec<String> {
    let input = serde_json::to_string(&json!({ "command": command })).unwrap();
    [
        json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}},"parent_tool_use_id":null,"session_id":"s","uuid":"u1"}),
        json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input}},"parent_tool_use_id":null,"session_id":"s","uuid":"u2"}),
        json!({"type":"stream_event","event":{"type":"content_block_stop","index":0},"parent_tool_use_id":null,"session_id":"s","uuid":"u3"}),
    ]
    .iter()
    .map(Value::to_string)
    .collect()
}

fn final_summary(command: &str) -> String {
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = feed_all(&mut parser, &tool_call_frames(command));
    finals(&parsed)
        .iter()
        .find_map(|event| match event {
            AdapterEvent::BlockFinal {
                summary: Some(summary),
                ..
            } => Some(summary.clone()),
            _ => None,
        })
        .expect("tool call has a summary")
}

#[test]
fn tool_summary_cuts_cyrillic_on_a_char_boundary() {
    // «Bash: a» занимает 7 байт, поэтому байт 120 приходится на середину буквы.
    let command = format!("a{}", "я".repeat(100));
    let summary = final_summary(&command);
    assert_eq!(summary, format!("Bash: a{}", "я".repeat(56)));
}

#[test]
fn tool_summary_cuts_a_four_byte_char_at_the_limit() {
    for padding in 0..4 {
        let command = format!("{}{}", "a".repeat(111 + padding), "😀".repeat(4));
        let summary = final_summary(&command);
        assert!(summary.len() <= 120, "{} bytes", summary.len());
        assert!(summary.len() > 116, "{} bytes", summary.len());
        assert!(summary.starts_with("Bash: a"));
    }
}

fn todos(parsed: &[Parsed]) -> Vec<(Option<Uuid>, Vec<TodoItem>)> {
    parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::TodoUpdated { lane, items }) => {
                Some((*lane, items.clone()))
            }
            _ => None,
        })
        .collect()
}

fn todo(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        text: text.to_owned(),
        status,
    }
}

// Форма настоящего кадра из todo_write.jsonl; отличаются только дорожка и результат.
fn todo_result(parent: Option<&str>, result: Value) -> String {
    json!({
        "type": "user",
        "parent_tool_use_id": parent,
        "session_id": "11111111-1111-4111-8111-111111111111",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000099",
        "message": {"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "toolu_todo2", "content": "Todos have been modified successfully."}]},
        "tool_use_result": result,
    })
    .to_string()
}

fn new_todos(list: Value) -> Value {
    json!({"oldTodos": [], "newTodos": list})
}

#[test]
fn real_todo_write_gives_one_full_list() {
    let lines = fixtures::replay_fixture("todo_write");
    let parsed = feed_all(&mut Parser::new(), &lines);
    assert_eq!(
        todos(&parsed),
        vec![(
            None,
            vec![
                todo("Write the first note", TodoStatus::InProgress),
                todo("Write the second note", TodoStatus::Pending),
            ]
        )]
    );
}

#[test]
fn subagent_todo_write_goes_to_the_lane_of_its_call() {
    let mut parser = Parser::new();
    let lines = fixtures::replay_fixture("subagent");
    let parsed = feed_all(&mut parser, &lines);
    let lane = parsed
        .iter()
        .find_map(|p| match p {
            Parsed::ToolCallComplete {
                block_id,
                tool_name,
                ..
            } if tool_name == "Task" => Some(*block_id),
            _ => None,
        })
        .expect("Task call");
    let list = json!([{"content": "Read", "status": "completed", "activeForm": "Reading"}]);
    let parsed = parser.feed(&todo_result(Some("toolu_task9"), new_todos(list)));
    assert_eq!(
        todos(&parsed),
        vec![(Some(lane), vec![todo("Read", TodoStatus::Completed)])]
    );
}

#[test]
fn todo_of_an_unknown_subagent_never_lands_in_the_main_lane() {
    let list = json!([{"content": "Read", "status": "pending", "activeForm": "Reading"}]);
    let parsed = Parser::new().feed(&todo_result(Some("toolu_lost"), new_todos(list)));
    assert_eq!(todos(&parsed), vec![]);
    assert!(
        parsed.iter().any(|p| matches!(
            p,
            Parsed::Event(AdapterEvent::Diagnostic {
                kind: "todo_lane_unknown",
                required: false,
                ..
            })
        )),
        "{parsed:?}"
    );
}

#[test]
fn rejected_todo_write_changes_nothing() {
    let parsed = Parser::new().feed(&todo_result(
        None,
        json!("Error: InputValidationError: todos"),
    ));
    assert_eq!(todos(&parsed), vec![]);
}

#[test]
fn empty_list_clears_and_odd_items_do_not_break_the_list() {
    let parsed = Parser::new().feed(&todo_result(None, new_todos(json!([]))));
    assert_eq!(todos(&parsed), vec![(None, vec![])]);
    let list = json!([
        {"content": "Later", "status": "blocked"},
        {"status": "pending"},
        {"content": 7, "status": "pending"},
        {"content": "Now", "status": "in_progress"},
    ]);
    let parsed = Parser::new().feed(&todo_result(None, new_todos(list)));
    assert_eq!(
        todos(&parsed),
        vec![(
            None,
            vec![
                todo("Later", TodoStatus::Pending),
                todo("Now", TodoStatus::InProgress),
            ]
        )]
    );
}

#[test]
fn todo_text_stays_out_of_debug() {
    let marker = "PRIVATE_TODO_MARKER";
    let list = json!([{"content": marker, "status": "pending", "activeForm": marker}]);
    let parsed = Parser::new().feed(&todo_result(None, new_todos(list)));
    assert!(!format!("{parsed:?}").contains(marker), "{parsed:?}");
    assert_eq!(todos(&parsed).len(), 1);
}

// `\u0000` в JSON — законная строка: разбор его не снимает, поэтому заменяет его хаб.
#[test]
fn json_nul_reaches_block_text_from_tool_results_and_text_deltas() {
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let parsed = parser.feed(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"a\u0000b"}]}}"#,
    );
    let final_text = finals(&parsed)
        .iter()
        .find_map(|event| match event {
            AdapterEvent::BlockFinal { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(final_text, "a\0b");

    parser.feed(
        r#"{"type":"stream_event","event":{"content_block":{"text":"","type":"text"},"index":0,"type":"content_block_start"}}"#,
    );
    let parsed = parser.feed(
        r#"{"type":"stream_event","event":{"delta":{"text":"c\u0000d","type":"text_delta"},"index":0,"type":"content_block_delta"}}"#,
    );
    assert!(
        matches!(
            deltas(&parsed)[0],
            Parsed::Event(AdapterEvent::BlockDelta { append, .. }) if append == "c\0d"
        ),
        "{parsed:?}"
    );
}

fn tool_final(tool: &str, input: Value) -> (Option<String>, String) {
    let mut parser = Parser::new();
    parser.set_input_command(command_id());
    let id = Uuid::now_v7();
    let start = json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":id.to_string(),"name":tool}}});
    let delta = json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}});
    let stop = json!({"type":"stream_event","event":{"type":"content_block_stop","index":0}});
    let parsed = feed_all(
        &mut parser,
        &[start.to_string(), delta.to_string(), stop.to_string()],
    );
    finals(&parsed)
        .into_iter()
        .find_map(|event| match event {
            AdapterEvent::BlockFinal { summary, text, .. } => Some((summary.clone(), text.clone())),
            _ => None,
        })
        .expect("tool final")
}

#[test]
fn command_label_is_its_first_line_without_the_heredoc() {
    let body = "cat > note.txt <<'EOF'\nсекретный текст\nEOF\nls";
    let (summary, _) = tool_final("Bash", json!({"command": body}));
    assert_eq!(summary.as_deref(), Some("Bash: cat > note.txt"));
    let (summary, _) = tool_final("Bash", json!({"command": "\n  cargo test\n  cargo build"}));
    assert_eq!(summary.as_deref(), Some("Bash: cargo test"));
}

#[test]
fn file_label_keeps_the_end_of_the_path() {
    let path = format!("/home/owner/{}/parser.rs", "deep/".repeat(40));
    let (summary, _) = tool_final("Read", json!({"file_path": path}));
    let summary = summary.unwrap();
    assert!(summary.starts_with("Read: …"), "{summary}");
    assert!(summary.ends_with("/parser.rs"), "{summary}");
    assert!(summary.len() <= 120, "{}", summary.len());
}

#[test]
fn tools_without_a_label_now_have_one() {
    let label = |tool: &str, input: Value| tool_final(tool, input).0.unwrap();
    assert_eq!(
        label("Skill", json!({"skill": "superpowers:writing-plans"})),
        "Skill: superpowers:writing-plans"
    );
    assert_eq!(
        label("ToolSearch", json!({"query": "select:Read"})),
        "ToolSearch: select:Read"
    );
    assert_eq!(
        label("WebFetch", json!({"url": "https://example.org/a"})),
        "WebFetch: https://example.org/a"
    );
    let question = json!({"questions": [{"question": "Продолжать?", "header": "Проверка"}]});
    assert_eq!(
        label("AskUserQuestion", question),
        "AskUserQuestion: Продолжать?"
    );
}

#[test]
fn call_block_carries_the_whole_input() {
    let command = format!("printf '{}'\necho done", "я".repeat(200));
    let (_, text) = tool_final("Bash", json!({"command": command}));
    assert_eq!(text, command);
    let (_, text) = tool_final("Read", json!({"file_path": "/tmp/a"}));
    assert_eq!(text, json!({"file_path": "/tmp/a"}).to_string());
}

fn unknown_names(parsed: &[Parsed]) -> Vec<String> {
    parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::Diagnostic {
                kind: "unknown_event",
                event_type,
                required: false,
                ..
            }) => Some(event_type.clone()),
            _ => None,
        })
        .collect()
}

fn stream(event: Value) -> String {
    json!({"type": "stream_event", "event": event}).to_string()
}

fn stems(dir: &str) -> Vec<String> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut names: Vec<String> = std::fs::read_dir(path)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "jsonl").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    names
}

#[test]
fn recorded_frames_raise_no_unknown_diagnostics() {
    let mut checked = 0;
    for name in stems("../../docs/stage0/evidence/claude-code") {
        let mut parser = Parser::new();
        let parsed = feed_all(&mut parser, &fixtures::replay_evidence(&name).out);
        assert_eq!(
            unknown_names(&parsed),
            Vec::<String>::new(),
            "evidence {name}"
        );
        checked += 1;
    }
    for name in stems("tests/fixtures/claude_code") {
        let mut parser = Parser::new();
        let parsed = feed_all(&mut parser, &fixtures::replay_fixture(&name));
        assert_eq!(
            unknown_names(&parsed),
            Vec::<String>::new(),
            "fixture {name}"
        );
        checked += 1;
    }
    assert!(checked > 15, "replayed {checked} files");
}

#[test]
fn unknown_top_level_type_is_diagnosed_once() {
    let mut parser = Parser::new();
    let frame = r#"{"type":"future_frame","payload":1}"#;
    assert_eq!(unknown_names(&parser.feed(frame)), ["future_frame"]);
    assert_eq!(unknown_names(&parser.feed(frame)), Vec::<String>::new());
    assert_eq!(
        unknown_names(&parser.feed(r#"{"type":"other_frame"}"#)),
        ["other_frame"]
    );
    parser.reset_process();
    assert_eq!(unknown_names(&parser.feed(frame)), Vec::<String>::new());
}

#[test]
fn unknown_system_subtype_is_diagnosed_once() {
    let mut parser = Parser::new();
    let frame = r#"{"type":"system","subtype":"future_notice"}"#;
    assert_eq!(unknown_names(&parser.feed(frame)), ["system/future_notice"]);
    assert_eq!(unknown_names(&parser.feed(frame)), Vec::<String>::new());
    let known = r#"{"type":"system","subtype":"task_started"}"#;
    assert_eq!(unknown_names(&parser.feed(known)), Vec::<String>::new());
}

#[test]
fn unknown_protocol_kinds_inside_a_stream_are_diagnosed() {
    let mut parser = Parser::new();
    let cases = [
        (
            stream(json!({"type": "future_event"})),
            "stream_event/future_event",
        ),
        (
            stream(json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "future_block"}})),
            "content_block/future_block",
        ),
        (
            stream(json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "future_delta"}})),
            "delta/future_delta",
        ),
    ];
    for (line, name) in cases {
        assert_eq!(unknown_names(&parser.feed(&line)), [name], "{name}");
        assert_eq!(unknown_names(&parser.feed(&line)), Vec::<String>::new());
    }
    for known in [
        stream(json!({"type": "message_stop"})),
        stream(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "signature_delta", "signature": ""}})),
    ] {
        assert_eq!(unknown_names(&parser.feed(&known)), Vec::<String>::new());
    }
}

#[test]
fn unknown_blocks_of_whole_frames_are_diagnosed() {
    let mut parser = Parser::new();
    parser.feed(&stream(
        json!({"type": "message_start", "message": {"id": "m"}}),
    ));
    let subagent = json!({"type": "assistant", "parent_tool_use_id": "toolu_1",
        "message": {"id": "m1", "content": [
            {"type": "text", "text": "ok"},
            {"type": "redacted_thinking", "data": "x"}]}})
    .to_string();
    assert_eq!(
        unknown_names(&parser.feed(&subagent)),
        ["assistant_block/redacted_thinking"]
    );
    let user =
        |kind: &str| json!({"type": "user", "message": {"content": [{"type": kind}]}}).to_string();
    assert_eq!(
        unknown_names(&parser.feed(&user("document"))),
        ["user_block/document"]
    );
    for known in ["text", "image"] {
        assert_eq!(
            unknown_names(&parser.feed(&user(known))),
            Vec::<String>::new()
        );
    }
}

#[test]
fn unknown_type_names_are_bounded() {
    let mut parser = Parser::new();
    let long = "x".repeat(500);
    let names = unknown_names(&parser.feed(&json!({"type": long}).to_string()));
    assert_eq!(names, ["x".repeat(64)]);
    let mut reported = 1;
    for n in 0..200 {
        reported +=
            unknown_names(&parser.feed(&json!({"type": format!("t{n}")}).to_string())).len();
    }
    assert_eq!(reported, 64);
}
