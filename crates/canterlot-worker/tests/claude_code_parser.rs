#![forbid(unsafe_code)]

//! Stream-JSON parser tests (plan Task 2, Step 1). Recorded fixtures come
//! from docs/stage0/evidence/claude-code/; constructed ones live in
//! tests/fixtures/claude_code/ (see its README.md).

use canterlot_worker::adapter::{
    AdapterEvent, BlockKind, Phase,
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
