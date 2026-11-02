#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Перевод событий OpenCode (L0) на записях этапа 0.

use canterlot_proto::limits::BLOCK_FINAL_INLINE_MAX_BYTES;
use canterlot_worker::adapter::{
    AdapterEvent, BlockKind, MediaSource, ModelUsage, Phase, TodoItem, TodoStatus,
    opencode::{
        VERIFIED_OPENAPI_SHA256, catalog,
        events::{RunFailure, RunOutcome, Translated, Translator, block_id},
        replay::capture_lines,
        version,
    },
};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

fn stream(name: &str) -> Vec<Value> {
    capture_lines(name, None)
        .into_iter()
        .filter(|line| line["dir"] == "sse")
        .map(|line| line["data"].clone())
        .collect()
}

fn capture_events(name: &str, scenario: Option<&str>) -> Vec<Value> {
    capture_lines(name, scenario)
        .into_iter()
        .filter(|line| line["dir"] == "sse")
        .map(|line| line["data"].clone())
        .collect()
}

fn session_of(events: &[Value]) -> String {
    events
        .iter()
        .find(|event| event["type"] == "session.created")
        .and_then(|event| event["data"]["sessionID"].as_str())
        .unwrap()
        .to_owned()
}

fn translator(events: &[Value]) -> (Translator, Uuid) {
    let windows = HashMap::from([("fake/fake-1".to_owned(), 200_000)]);
    let mut translator = Translator::new(&session_of(events), windows);
    let command = Uuid::now_v7();
    translator.begin_run(Some(command));
    (translator, command)
}

fn feed_all(translator: &mut Translator, events: &[Value]) -> Vec<Translated> {
    events
        .iter()
        .flat_map(|event| translator.feed(event))
        .collect()
}

fn adapter_events(translated: Vec<Translated>) -> Vec<AdapterEvent> {
    translated
        .into_iter()
        .filter_map(|item| match item {
            Translated::Event(event) => Some(event),
            _ => None,
        })
        .collect()
}

#[test]
fn recorded_turn_gives_independent_text_and_reasoning_blocks() {
    let events = stream("start_stream");
    let (mut translator, command) = translator(&events);
    let out = adapter_events(feed_all(&mut translator, &events));
    let mut started = Vec::new();
    let mut finals = HashMap::new();
    let mut deltas: HashMap<Uuid, String> = HashMap::new();
    for event in &out {
        match event {
            AdapterEvent::BlockStarted {
                block_id,
                kind,
                input_command_id,
                agent,
                model,
                ..
            } => {
                assert_eq!(*input_command_id, Some(command));
                assert_eq!(agent, "orchestrator");
                assert_eq!(model, "fake/fake-1");
                started.push((*block_id, format!("{kind:?}")));
            }
            AdapterEvent::BlockDelta { block_id, append } => {
                deltas.entry(*block_id).or_default().push_str(append);
            }
            AdapterEvent::BlockFinal { block_id, text, .. } => {
                finals.insert(*block_id, text.clone());
            }
            _ => {}
        }
    }
    assert_eq!(
        started
            .iter()
            .map(|(_, kind)| kind.as_str())
            .collect::<Vec<_>>(),
        ["Reasoning", "Text"]
    );
    let (reasoning, text) = (started[0].0, started[1].0);
    assert_eq!(finals[&reasoning], "think more");
    assert_eq!(finals[&text], "Hello world");
    assert_eq!(deltas[&reasoning], "think more");
    assert_eq!(deltas[&text], "Hello world");
}

// Манифест версии: постоянная адаптера и запись этапа 0 не должны разойтись молча.
#[test]
fn verified_schema_digest_is_the_recorded_one() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/stage0/evidence/opencode/openapi-2.1.4.sha256"),
    )
    .unwrap();
    let runtime = manifest
        .lines()
        .find(|line| line.contains("GET /openapi.json"))
        .and_then(|line| line.split_whitespace().next())
        .unwrap();
    assert_eq!(runtime, VERIFIED_OPENAPI_SHA256);
    assert!(
        version::VERIFIED_VERSIONS
            .iter()
            .any(|(major, minor, patch)| format!("{major}.{minor}.{patch}") == "2.1.4")
    );
}

#[test]
fn step_start_means_generating() {
    let events = stream("start_stream");
    let (mut translator, _) = translator(&events);
    let out = adapter_events(feed_all(&mut translator, &events));
    assert!(out.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase {
            phase: Phase::Generating,
            ..
        }
    )));
}

#[test]
fn block_ids_are_stable_for_live_events_and_reconciliation() {
    let events = stream("start_stream");
    let session = session_of(&events);
    let message = events
        .iter()
        .find(|event| event["type"] == "session.text.started")
        .and_then(|event| event["data"]["assistantMessageID"].as_str())
        .unwrap();
    let id = block_id(&session, message, "text", "0");
    assert_eq!(id, block_id(&session, message, "text", "0"));
    assert_ne!(id, block_id(&session, message, "reasoning", "0"));
    assert_ne!(id, block_id(&session, message, "text", "1"));
    assert_ne!(id, block_id("ses_other", message, "text", "0"));
    let (mut translator, _) = translator(&events);
    let out = adapter_events(feed_all(&mut translator, &events));
    assert!(out.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { block_id, kind: BlockKind::Text, .. } if *block_id == id
    )));
}

#[test]
fn usage_keeps_the_recorded_token_split() {
    let events = stream("start_stream");
    let (mut translator, command) = translator(&events);
    let out = feed_all(&mut translator, &events);
    assert!(
        out.iter()
            .any(|item| matches!(item, Translated::RunEnded(RunOutcome::Succeeded)))
    );
    let AdapterEvent::RunFinished {
        input_command_id,
        usage,
        context_tokens,
        context_window_tokens,
    } = translator.finish_run(command)
    else {
        panic!("not a run end");
    };
    assert_eq!(input_command_id, command);
    assert_eq!(
        usage,
        vec![ModelUsage {
            model: "fake/fake-1".into(),
            agent: "orchestrator".into(),
            input_tokens: 70,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 0,
        }]
    );
    assert_eq!(context_tokens, 100);
    assert_eq!(context_window_tokens, 200_000);
}

#[test]
fn usage_of_several_steps_adds_up_and_context_is_the_last_step() {
    let events = stream("tools");
    let (mut translator, command) = translator(&events);
    feed_all(&mut translator, &events);
    let AdapterEvent::RunFinished {
        usage,
        context_tokens,
        ..
    } = translator.finish_run(command)
    else {
        panic!("not a run end");
    };
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].input_tokens, 350);
    assert_eq!(usage[0].output_tokens, 100);
    assert_eq!(usage[0].cache_read_tokens, 150);
    assert_eq!(context_tokens, 100);
}

#[test]
fn failed_step_tokens_count_and_usage_resets_between_runs() {
    let session = "ses_a";
    let mut translator = Translator::new(session, HashMap::new());
    let first = Uuid::now_v7();
    translator.begin_run(Some(first));
    let step = |kind: &str, tokens: Value| {
        json!({"type": kind, "data": {"sessionID": session, "assistantMessageID": "msg_1",
            "agent": "build", "model": {"id": "m", "providerID": "p"}, "tokens": tokens}})
    };
    let tokens = json!({"input": 1, "output": 2, "reasoning": 3, "cache": {"read": 4, "write": 5}});
    translator.feed(&step("session.step.started", Value::Null));
    translator.feed(&step("session.step.failed", tokens));
    let AdapterEvent::RunFinished {
        usage,
        context_tokens,
        context_window_tokens,
        ..
    } = translator.finish_run(first)
    else {
        panic!("not a run end");
    };
    assert_eq!(usage[0].output_tokens, 5);
    assert_eq!(usage[0].cache_write_tokens, 5);
    assert_eq!(context_tokens, 10);
    assert_eq!(context_window_tokens, 0);
    let second = Uuid::now_v7();
    translator.begin_run(Some(second));
    let AdapterEvent::RunFinished { usage, .. } = translator.finish_run(second) else {
        panic!("not a run end");
    };
    assert!(usage.is_empty());
}

#[test]
fn failed_step_without_tokens_is_harmless() {
    let events = stream("service_failure");
    let session = events
        .iter()
        .find(|event| event["type"] == "session.step.failed")
        .and_then(|event| event["data"]["sessionID"].as_str())
        .unwrap()
        .to_owned();
    let mut translator = Translator::new(&session, HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    let out = feed_all(&mut translator, &events);
    assert!(out.iter().any(|item| matches!(
        item,
        Translated::RunEnded(RunOutcome::Failed(failure)) if failure.kind == "provider.auth"
    )));
}

#[test]
fn other_sessions_are_dropped_and_children_are_remembered() {
    let events = stream("start_stream");
    let mut translator = Translator::new("ses_someone_else", HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    assert!(adapter_events(feed_all(&mut translator, &events)).is_empty());
    assert!(translator.owns("ses_someone_else"));
    assert!(!translator.owns("ses_child"));
    translator.feed(&json!({"type": "session.created",
        "data": {"sessionID": "ses_child", "parentID": "ses_someone_else"}}));
    assert!(translator.owns("ses_child"));
    translator.feed(&json!({"type": "session.created",
        "data": {"sessionID": "ses_grandchild", "parentID": "ses_child"}}));
    assert!(translator.owns("ses_grandchild"));
}

#[test]
fn inbox_events_name_the_input() {
    let events = stream("start_stream");
    let (mut translator, _) = translator(&events);
    let seen: Vec<_> = feed_all(&mut translator, &events)
        .into_iter()
        .filter_map(|item| match item {
            Translated::InboxSeen { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(
        seen,
        [
            "msg_18f9dc297001Uo9jYOKSsV0hur",
            "msg_18f9dc297001Uo9jYOKSsV0hur"
        ]
    );
}

#[test]
fn every_recorded_event_type_is_known() {
    for name in [
        "compaction",
        "crash",
        "delivery_ack",
        "delivery_console",
        "image_input",
        "instructions",
        "interrupt",
        "mcp",
        "models",
        "permissions",
        "question",
        "real_free_model",
        "reconnect",
        "resume",
        "service_failure",
        "shared_db",
        "start_stream",
        "stuck_invalid_request",
        "subagent",
        "todo",
        "tools",
    ] {
        let events = stream(name);
        let root = events
            .iter()
            .find_map(|event| event["data"]["sessionID"].as_str())
            .unwrap_or("ses_none")
            .to_owned();
        let mut translator = Translator::new(&root, HashMap::new());
        translator.begin_run(Some(Uuid::now_v7()));
        for event in &events {
            for item in translator.feed(event) {
                assert!(
                    !matches!(item, Translated::Event(AdapterEvent::Diagnostic { .. })),
                    "{name}: {event}"
                );
            }
        }
    }
}

#[test]
fn unknown_type_is_diagnosed_once() {
    let mut translator = Translator::new("ses_a", HashMap::new());
    let event = json!({"type": "session.brand.new", "data": {"sessionID": "ses_a"}});
    let first = translator.feed(&event);
    assert!(matches!(
        first.as_slice(),
        [Translated::Event(AdapterEvent::Diagnostic {
            kind: "unknown_event",
            required: false,
            ..
        })]
    ));
    assert!(translator.feed(&event).is_empty());
}

#[test]
fn unknown_execution_end_is_required() {
    let mut translator = Translator::new("ses_a", HashMap::new());
    let unknown = json!({"type": "session.execution.paused", "data": {"sessionID": "ses_a"}});
    assert!(matches!(
        translator.feed(&unknown).as_slice(),
        [Translated::Event(AdapterEvent::Diagnostic {
            required: true,
            ..
        })]
    ));
    let broken = json!({"type": "session.execution.succeeded", "data": {}});
    assert!(matches!(
        translator.feed(&broken).as_slice(),
        [Translated::Event(AdapterEvent::Diagnostic {
            required: true,
            ..
        })]
    ));
}

#[test]
fn delta_without_a_start_opens_the_block() {
    let mut translator = Translator::new("ses_a", HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    let out = adapter_events(translator.feed(&json!({"type": "session.text.delta",
        "data": {"sessionID": "ses_a", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "Привет"}})));
    assert!(matches!(out[0], AdapterEvent::BlockStarted { .. }));
    assert!(matches!(&out[1], AdapterEvent::BlockDelta { append, .. } if append == "Привет"));
}

#[test]
fn debug_never_shows_text() {
    let failure = json!({"type": "session.execution.failed", "data": {"sessionID": "ses_a",
        "error": {"type": "provider.auth", "message": "секретный текст ошибки"}}});
    let mut translator = Translator::new("ses_a", HashMap::new());
    let out = translator.feed(&failure);
    let shown = format!("{out:?}");
    assert!(!shown.contains("секретный"), "{shown}");
}

#[test]
fn context_windows_come_from_the_recorded_catalog() {
    let list = capture_lines("models", None)
        .into_iter()
        .filter(|line| line["dir"] == "http_in")
        .map(|line| line["data"]["body"].clone())
        .find(|body| body["data"][0]["providerID"].is_string())
        .unwrap();
    let windows = catalog::context_windows(&list);
    assert!(!windows.is_empty());
    assert!(windows.keys().all(|model| model.contains('/')));
    assert!(windows.values().all(|window| *window > 0));
}

struct ToolView {
    name: String,
    summary: String,
    call: Uuid,
    result_text: Option<String>,
    final_summary: Option<String>,
}

fn tools_of(translated: Vec<Translated>) -> Vec<ToolView> {
    let events: Vec<AdapterEvent> = translated
        .into_iter()
        .filter_map(|item| match item {
            Translated::ToolCall { event, .. } | Translated::Event(event) => Some(event),
            _ => None,
        })
        .collect();
    let mut tools = Vec::new();
    for event in &events {
        let AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::ToolCall,
            tool_name,
            summary,
            ..
        } = event
        else {
            continue;
        };
        let result = events.iter().find_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id: result,
                kind: BlockKind::ToolResult,
                parent,
                ..
            } if *parent == Some(*block_id) => Some(*result),
            _ => None,
        });
        let final_of = |wanted: Uuid| {
            events.iter().find_map(|event| match event {
                AdapterEvent::BlockFinal {
                    block_id,
                    text,
                    summary,
                } if *block_id == wanted => Some((text.clone(), summary.clone())),
                _ => None,
            })
        };
        tools.push(ToolView {
            name: tool_name.clone().unwrap_or_default(),
            summary: summary.clone().unwrap_or_default(),
            call: *block_id,
            result_text: result.and_then(final_of).map(|(text, _)| text),
            final_summary: final_of(*block_id).and_then(|(_, summary)| summary),
        });
    }
    tools
}

#[test]
fn recorded_tools_pair_with_their_results() {
    let events = stream("tools");
    let (mut translator, _) = translator(&events);
    let tools = tools_of(feed_all(&mut translator, &events));
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        ["shell", "read", "shell", "read", "write", "edit", "shell"]
    );
    assert_eq!(tools[0].summary, "shell: echo hi; echo err >&2");
    assert_eq!(tools[0].result_text.as_deref(), Some("hi\nerr\n"));
    assert_eq!(tools[1].summary, "read: note.txt");
    assert!(tools[1].result_text.as_deref().unwrap().contains("alpha"));
    assert!(tools.iter().all(|tool| tool.result_text.is_some()));
    assert!(tools.iter().all(|tool| !tool.call.is_nil()));
}

// `success` с ненулевым выходом для показа — упавшая команда.
#[test]
fn shell_exit_status_reaches_the_summary() {
    let events = stream("tools");
    let (mut translator, _) = translator(&events);
    let tools = tools_of(feed_all(&mut translator, &events));
    assert_eq!(
        tools[0].final_summary.as_deref(),
        Some("shell: echo hi; echo err >&2, exit 0")
    );
    assert_eq!(
        tools[2].final_summary.as_deref(),
        Some("shell: exit 3, exit 3")
    );
    assert_eq!(
        tools[1].final_summary.as_deref(),
        Some("read: note.txt, completed")
    );
}

// Сосед завершился раньше первого вызова шага: связь идёт по паре (сообщение, id).
#[test]
fn parallel_failure_before_its_sibling_keeps_each_result_in_place() {
    let events = stream("tools");
    let (mut translator, _) = translator(&events);
    let tools = tools_of(feed_all(&mut translator, &events));
    assert_eq!(
        tools[3].final_summary.as_deref(),
        Some("read: missing.txt, failed")
    );
    assert_eq!(
        tools[3].result_text.as_deref(),
        Some("File not found: missing.txt")
    );
    assert!(
        tools[2]
            .result_text
            .as_deref()
            .unwrap()
            .contains("Exited with code 3")
    );
}

#[test]
fn rejection_and_interruption_are_named() {
    let translated = |name: &str| {
        let events = stream(name);
        let session = events
            .iter()
            .find(|event| event["type"] == "session.tool.failed")
            .and_then(|event| event["data"]["sessionID"].as_str())
            .unwrap()
            .to_owned();
        let mut translator = Translator::new(&session, HashMap::new());
        translator.begin_run(Some(Uuid::now_v7()));
        tools_of(feed_all(&mut translator, &events))
    };
    let summaries: Vec<_> = translated("permissions")
        .into_iter()
        .filter_map(|tool| tool.final_summary)
        .collect();
    assert!(
        summaries
            .iter()
            .any(|summary| summary.ends_with(", declined")),
        "{summaries:?}"
    );
    let summaries: Vec<_> = translated("interrupt")
        .into_iter()
        .filter_map(|tool| tool.final_summary)
        .collect();
    assert!(
        summaries
            .iter()
            .any(|summary| summary.ends_with(", interrupted")),
        "{summaries:?}"
    );
}

#[test]
fn running_tool_phase_names_the_tool() {
    let events = stream("tools");
    let (mut translator, _) = translator(&events);
    let out = adapter_events(feed_all(&mut translator, &events));
    assert!(out.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase { phase: Phase::RunningTool, tool_name: Some(name), .. } if name == "shell"
    )));
}

#[test]
fn tool_events_carry_the_item_for_the_mark() {
    let events = stream("tools");
    let (mut translator, _) = translator(&events);
    let out = feed_all(&mut translator, &events);
    let calls = out
        .iter()
        .filter(|item| matches!(item, Translated::ToolCall { .. }))
        .count();
    let done = out
        .iter()
        .filter(|item| matches!(item, Translated::ToolDone(_)))
        .count();
    let running = out
        .iter()
        .filter(|item| matches!(item, Translated::ToolRunning(_)))
        .count();
    assert_eq!((calls, done, running), (7, 7, 3));
}

// Имя есть только у `input.started`; вызов без него не теряет ни блок, ни результат.
#[test]
fn result_without_a_start_still_opens_the_call() {
    let mut translator = Translator::new("ses_a", HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    let out = translator.feed(&json!({"type": "session.tool.failed", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_1", "id": "call_1",
        "error": {"type": "tool.execution", "message": "Ошибка инструмента"}}}));
    let tools = tools_of(out);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].result_text.as_deref(), Some("Ошибка инструмента"));
}

#[test]
fn long_cyrillic_command_is_cut_on_a_character_boundary() {
    let mut translator = Translator::new("ses_a", HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    translator.feed(&json!({"type": "session.tool.input.started", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_1", "id": "call_1", "name": "shell"}}));
    let command = "э".repeat(4000);
    let out = translator.feed(&json!({"type": "session.tool.called", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_1", "id": "call_1",
        "input": {"command": command}}}));
    let tools = tools_of(out);
    assert!(tools[0].summary.len() <= 256);
    assert!(tools[0].summary.ends_with("э…"), "a cut label is marked");
}

#[test]
fn permission_and_form_requests_name_their_session_and_call() {
    let events = stream("permissions");
    let session = events
        .iter()
        .find(|event| event["type"] == "permission.asked")
        .and_then(|event| event["data"]["sessionID"].as_str())
        .unwrap()
        .to_owned();
    let mut translator = Translator::new(&session, HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    let request = feed_all(&mut translator, &events)
        .into_iter()
        .find_map(|item| match item {
            Translated::Request(request) => Some(request),
            _ => None,
        })
        .unwrap();
    assert!(request.id.starts_with("per_"));
    assert_eq!(request.session, session);
    assert!(request.item.is_some());

    let events = stream("question");
    let session = events
        .iter()
        .find(|event| event["type"] == "form.created")
        .and_then(|event| event["data"]["form"]["sessionID"].as_str())
        .unwrap()
        .to_owned();
    let mut translator = Translator::new(&session, HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    let request = feed_all(&mut translator, &events)
        .into_iter()
        .find_map(|item| match item {
            Translated::Request(request) => Some(request),
            _ => None,
        })
        .unwrap();
    assert!(request.id.starts_with("frm_"));
    assert_eq!(request.session, session);
}

#[test]
fn debug_of_tool_events_hides_arguments_and_output() {
    let events = stream("tools");
    let (mut translator, _) = translator(&events);
    let shown = format!("{:?}", feed_all(&mut translator, &events));
    assert!(!shown.contains("echo hi"), "{shown}");
    assert!(!shown.contains("alpha"), "{shown}");
}

// Запись настоящего сервера: картинка лежит в результате инструмента `read` как `data:` URI.
#[test]
fn picture_in_a_tool_result_goes_to_the_call_block() {
    let events = capture_events("recovery", Some("image_read"));
    let (mut translator, command) = translator(&events);
    translator.begin_run(Some(command));
    let translated = feed_all(&mut translator, &events);
    let call = translated
        .iter()
        .find_map(|item| match item {
            Translated::ToolCall { block, .. } => Some(*block),
            _ => None,
        })
        .unwrap();
    let translated = adapter_events(translated);
    let media: Vec<_> = translated
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::Media { block_id, source } => Some((*block_id, source.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].0, call);
    assert!(
        matches!(&media[0].1, MediaSource::Base64(text) if text.starts_with("iVBORw0KGgo")),
        "{:?}",
        media[0].1
    );
    let texts: Vec<_> = translated
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockFinal { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().all(|text| !text.contains("base64")),
        "{texts:?}"
    );
}

fn tool_result(content: Value) -> Vec<Value> {
    let session = "ses_1";
    vec![
        json!({"type": "session.tool.input.started", "data": {
            "sessionID": session, "assistantMessageID": "msg_1", "id": "call_1", "name": "read"}}),
        json!({"type": "session.tool.called", "data": {
            "sessionID": session, "assistantMessageID": "msg_1", "id": "call_1", "input": {}}}),
        json!({"type": "session.tool.success", "data": {
            "sessionID": session, "assistantMessageID": "msg_1", "id": "call_1", "content": content}}),
    ]
}

// Только картинки: PDF в `read` приходит той же формой, а показать его негде.
#[test]
fn other_files_in_a_tool_result_are_not_pictures() {
    let events = tool_result(json!([
        {"type": "file", "uri": "data:application/pdf;base64,AAAA", "mime": "application/pdf"},
        {"type": "file", "uri": "file:///tmp/x.png", "mime": "image/png"},
    ]));
    let mut translator = Translator::new("ses_1", Default::default());
    let translated = adapter_events(feed_all(&mut translator, &events));
    assert!(
        !translated
            .iter()
            .any(|event| matches!(event, AdapterEvent::Media { .. }))
    );
}

// Текст крупнее финала доходит до хаба только дельтами, иначе блок останется пустым.
#[test]
fn long_tool_output_goes_as_a_delta_before_the_final() {
    let text = "я".repeat(BLOCK_FINAL_INLINE_MAX_BYTES);
    let events = tool_result(json!([{"type": "text", "text": text}]));
    let mut translator = Translator::new("ses_1", Default::default());
    let translated = adapter_events(feed_all(&mut translator, &events));
    let delta = translated.iter().find_map(|event| match event {
        AdapterEvent::BlockDelta { append, .. } => Some(append.clone()),
        _ => None,
    });
    assert_eq!(delta.as_deref(), Some(text.as_str()));
    let short = tool_result(json!([{"type": "text", "text": "коротко"}]));
    let mut translator = Translator::new("ses_1", Default::default());
    let translated = adapter_events(feed_all(&mut translator, &short));
    assert!(
        !translated
            .iter()
            .any(|event| matches!(event, AdapterEvent::BlockDelta { .. }))
    );
}

// Расход старых шагов уже учтён прошлым исполнением, а следующий разрыв читает тот же хвост.
#[test]
fn steps_seen_at_resume_are_not_counted_by_the_next_gap() {
    let step = |id: &str| {
        json!({
            "type": "assistant",
            "id": id,
            "model": {"id": "fake-1", "providerID": "fake"},
            "time": {"completed": 1},
            "tokens": {"input": 100, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}},
            "content": [],
        })
    };
    let old = step("msg_old");
    let mut translator = Translator::new("ses_1", HashMap::new());
    translator.replay(std::slice::from_ref(&old), true, false);
    let command = Uuid::now_v7();
    translator.begin_run(Some(command));
    translator.replay(&[old, step("msg_new")], false, true);
    let AdapterEvent::RunFinished { usage, .. } = translator.finish_run(command) else {
        panic!("finish");
    };
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(usage[0].input_tokens, 100);
    assert_eq!(usage[0].output_tokens, 10);
}

const SUBAGENT_CALL_MESSAGE: &str = "msg_18f9fff3a001cN2CMuCCo0DcTD";
const SUBAGENT_CALL: &str = "call_0_1793410989907";
const CHILD_STEP: &str = "msg_18f9fffa4001TOgiNGtYYfC18X";

struct Started {
    id: Uuid,
    kind: String,
    parent: Option<Uuid>,
    agent: String,
    command: Option<Uuid>,
}

fn started(out: &[AdapterEvent]) -> Vec<Started> {
    out.iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id,
                kind,
                parent,
                agent,
                input_command_id,
                ..
            } => Some(Started {
                id: *block_id,
                kind: format!("{kind:?}"),
                parent: *parent,
                agent: agent.clone(),
                command: *input_command_id,
            }),
            _ => None,
        })
        .collect()
}

// Обработчик инструментов отдаёт начало вызова отдельно: в ленту оно попадает через адаптер.
fn flat_events(translated: Vec<Translated>) -> Vec<AdapterEvent> {
    translated
        .into_iter()
        .filter_map(|item| match item {
            Translated::Event(event) | Translated::ToolCall { event, .. } => Some(event),
            _ => None,
        })
        .collect()
}

#[test]
fn child_blocks_go_into_the_lane_of_the_subagent_call() {
    let events = stream("subagent");
    let root = session_of(&events);
    let (mut translator, command) = translator(&events);
    let out = flat_events(feed_all(&mut translator, &events));
    let blocks = started(&out);
    let call = block_id(&root, SUBAGENT_CALL_MESSAGE, "tool", SUBAGENT_CALL);
    let child_reasoning = block_id(&root, CHILD_STEP, "reasoning", "0");
    let child_tool = block_id(&root, CHILD_STEP, "tool", "call_0_1793410990014");
    let child_result = block_id(&root, CHILD_STEP, "tool_result", "call_0_1793410990014");
    for id in [child_reasoning, child_tool] {
        let block = blocks.iter().find(|block| block.id == id).expect("block");
        assert_eq!(block.parent, Some(call), "{}", block.kind);
        assert_eq!(block.agent, "general");
        assert_eq!(block.command, Some(command));
    }
    let result = blocks
        .iter()
        .find(|block| block.id == child_result)
        .expect("result");
    assert_eq!(result.parent, Some(child_tool));
    assert_eq!(result.agent, "general");
    let own = blocks.iter().find(|block| block.id == call).expect("call");
    assert_eq!((own.parent, own.agent.as_str()), (None, "orchestrator"));
}

#[test]
fn background_child_gets_its_own_lane_and_agent() {
    let events = stream("subagent");
    let root = session_of(&events);
    let (mut translator, _) = translator(&events);
    let out = flat_events(feed_all(&mut translator, &events));
    let blocks = started(&out);
    let background = block_id(
        &root,
        "msg_18fa0006a001a9L2TIpCXYNl5y",
        "tool",
        "call_0_1793410990206",
    );
    let text = block_id(&root, "msg_18fa000d8001PvufCICZzuOtVQ", "text", "0");
    let block = blocks.iter().find(|block| block.id == text).expect("text");
    assert_eq!(block.parent, Some(background));
    assert_eq!(block.agent, "explore");
}

#[test]
fn child_activity_does_not_change_the_phase_of_the_main_agent() {
    let events = stream("subagent");
    let (mut translator, _) = translator(&events);
    let child_events: Vec<Value> = events
        .iter()
        .filter(|event| {
            event["data"]["sessionID"]
                .as_str()
                .is_some_and(|session| session.starts_with("ses_e7060007"))
                && event["type"] != "session.created"
        })
        .cloned()
        .collect();
    translator.feed(&events[1]);
    feed_all(
        &mut translator,
        &events
            .iter()
            .filter(|event| event["type"] == "session.created")
            .cloned()
            .collect::<Vec<_>>(),
    );
    let out = adapter_events(feed_all(&mut translator, &child_events));
    assert!(
        !out.iter()
            .any(|event| matches!(event, AdapterEvent::Phase { .. })),
        "{out:?}"
    );
}

#[test]
fn child_tokens_are_counted_under_its_agent_and_never_touch_the_context() {
    let events = stream("subagent");
    let (mut translator, command) = translator(&events);
    let until_parent_step: Vec<Value> = events
        .iter()
        .take_while(|event| event["data"]["assistantMessageID"] != "msg_18fa000b3001xwxVXXMjgMrquJ")
        .cloned()
        .collect();
    feed_all(&mut translator, &until_parent_step);
    let AdapterEvent::RunFinished {
        usage,
        context_tokens,
        ..
    } = translator.finish_run(command)
    else {
        panic!("run must finish");
    };
    let line = |agent: &str| {
        usage
            .iter()
            .find(|entry| entry.agent == agent)
            .unwrap_or_else(|| panic!("{agent}: {usage:?}"))
            .clone()
    };
    assert_eq!(
        line("general"),
        ModelUsage {
            model: "fake/fake-1".into(),
            agent: "general".into(),
            input_tokens: 140,
            output_tokens: 40,
            cache_read_tokens: 60,
            cache_write_tokens: 0,
        }
    );
    assert_eq!(line("orchestrator").input_tokens, 140, "two parent steps");
    assert_eq!(context_tokens, 100, "last step of the main agent");
}

#[test]
fn child_of_a_child_stays_in_its_parents_lane_chain() {
    let root = "ses_root";
    let mut translator = Translator::new(root, HashMap::new());
    translator.begin_run(Some(Uuid::now_v7()));
    let progress = |session: &str, message: &str, id: &str, child: &str| {
        json!({"type": "session.tool.progress", "data": {
            "sessionID": session, "assistantMessageID": message, "id": id,
            "metadata": {"sessionID": child, "status": "running"}}})
    };
    translator.feed(&json!({"type": "session.created", "data": {
        "sessionID": "ses_a", "parentID": root, "agent": "general"}}));
    translator.feed(&progress(root, "msg_1", "call_1", "ses_a"));
    translator.feed(&json!({"type": "session.created", "data": {
        "sessionID": "ses_b", "parentID": "ses_a", "agent": "explore"}}));
    translator.feed(&progress("ses_a", "msg_2", "call_2", "ses_b"));
    let out = flat_events(
        translator.feed(&json!({"type": "session.text.started", "data": {
        "sessionID": "ses_b", "assistantMessageID": "msg_3", "ordinal": 0}})),
    );
    let blocks = started(&out);
    assert_eq!(
        blocks[0].parent,
        Some(block_id(root, "msg_2", "tool", "call_2"))
    );
    assert_eq!(blocks[0].agent, "explore");
}

#[test]
fn result_of_the_call_names_a_child_whose_progress_was_lost() {
    let root = "ses_root";
    let mut translator = Translator::new(root, HashMap::new());
    translator.add_child("ses_a".to_owned());
    translator.feed(&json!({"type": "session.tool.input.started", "data": {
        "sessionID": root, "assistantMessageID": "msg_1", "id": "call_1", "name": "subagent"}}));
    translator.feed(&json!({"type": "session.tool.success", "data": {
        "sessionID": root, "assistantMessageID": "msg_1", "id": "call_1", "content": [],
        "metadata": {"sessionID": "ses_a", "status": "completed"}}}));
    let out = flat_events(
        translator.feed(&json!({"type": "session.text.started", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_2", "ordinal": 0}})),
    );
    assert_eq!(
        started(&out)[0].parent,
        Some(block_id(root, "msg_1", "tool", "call_1"))
    );
}

#[test]
fn child_with_an_unknown_lane_still_shows_its_blocks() {
    let root = "ses_root";
    let mut translator = Translator::new(root, HashMap::new());
    translator.add_child("ses_a".to_owned());
    let out = flat_events(
        translator.feed(&json!({"type": "session.text.started", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_2", "ordinal": 0}})),
    );
    let blocks = started(&out);
    assert_eq!(blocks[0].parent, None);
    assert_eq!(blocks[0].agent, "subagent");
}

#[test]
fn background_child_keeps_its_open_blocks_over_the_end_of_the_run() {
    let root = "ses_root";
    let mut translator = Translator::new(root, HashMap::new());
    let command = Uuid::now_v7();
    translator.begin_run(Some(command));
    translator.feed(&json!({"type": "session.created", "data": {
        "sessionID": "ses_a", "parentID": root, "agent": "general"}}));
    translator.feed(&json!({"type": "session.step.started", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_2", "agent": "general",
        "model": {"id": "other", "providerID": "fake"}}}));
    translator.feed(&json!({"type": "session.tool.input.started", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_2", "id": "call_1", "name": "shell"}}));
    translator.feed(&json!({"type": "session.tool.called", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_2", "id": "call_1",
        "input": {"command": "sleep 5"}}}));
    let _ = translator.finish_run(command);
    let out = flat_events(
        translator.feed(&json!({"type": "session.tool.success", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_2", "id": "call_1",
        "content": [{"type": "text", "text": "done"}], "metadata": {"exit": 0}}})),
    );
    let call = block_id(root, "msg_2", "tool", "call_1");
    assert!(
        !started(&out).iter().any(|block| block.id == call),
        "the call must not start twice: {out:?}"
    );
    let result = started(&out);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].kind, "ToolResult");
    assert_eq!(result[0].agent, "general");
}

type Lists = Vec<(Option<Uuid>, Vec<TodoItem>)>;

fn todo_lists(out: &[AdapterEvent]) -> Lists {
    out.iter()
        .filter_map(|event| match event {
            AdapterEvent::TodoUpdated { lane, items } => Some((*lane, items.clone())),
            _ => None,
        })
        .collect()
}

fn todo(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        text: text.into(),
        status,
    }
}

#[test]
fn recorded_todo_list_comes_whole_with_every_status() {
    let events = stream("todo");
    let (mut translator, _) = translator(&events);
    let out = flat_events(feed_all(&mut translator, &events));
    assert_eq!(
        todo_lists(&out),
        vec![
            (
                None,
                vec![
                    todo("first", TodoStatus::InProgress),
                    todo("second", TodoStatus::Pending)
                ]
            ),
            (
                None,
                vec![
                    todo("first", TodoStatus::Completed),
                    todo("second", TodoStatus::Cancelled)
                ]
            ),
        ]
    );
}

// Придуманный кадр: форма взята у основного агента, настоящий проверяет L2
// `real_child_todo_list_goes_to_the_lane_of_its_call`.
fn child_todo(session: &str) -> Vec<Value> {
    vec![
        json!({"type": "session.tool.input.started", "data": {
            "sessionID": session, "assistantMessageID": "msg_9", "id": "call_9",
            "name": "todowrite"}}),
        json!({"type": "session.tool.success", "data": {
            "sessionID": session, "assistantMessageID": "msg_9", "id": "call_9",
            "content": [{"type": "text", "text": "Todos updated"}],
            "metadata": {"todos": [{"content": "дочерний", "status": "in_progress"}]}}}),
    ]
}

#[test]
fn child_list_goes_to_the_lane_of_its_call() {
    let root = "ses_root";
    let mut translator = Translator::new(root, HashMap::new());
    translator.feed(&json!({"type": "session.created", "data": {
        "sessionID": "ses_a", "parentID": root, "agent": "general"}}));
    translator.feed(&json!({"type": "session.tool.progress", "data": {
        "sessionID": root, "assistantMessageID": "msg_1", "id": "call_1",
        "metadata": {"sessionID": "ses_a", "status": "running"}}}));
    let out = flat_events(feed_all(&mut translator, &child_todo("ses_a")));
    assert_eq!(
        todo_lists(&out),
        vec![(
            Some(block_id(root, "msg_1", "tool", "call_1")),
            vec![todo("дочерний", TodoStatus::InProgress)]
        )]
    );
}

#[test]
fn child_list_without_a_lane_is_diagnosed_and_never_replaces_the_main_list() {
    let root = "ses_root";
    let mut translator = Translator::new(root, HashMap::new());
    translator.add_child("ses_a".to_owned());
    let out = flat_events(feed_all(&mut translator, &child_todo("ses_a")));
    assert!(todo_lists(&out).is_empty());
    assert!(out.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "todo_lane_unknown",
            required: false,
            ..
        }
    )));
}

#[test]
fn list_seen_live_is_not_sent_again_by_the_projection() {
    let events = stream("todo");
    let (mut translator, _) = translator(&events);
    let first = flat_events(feed_all(&mut translator, &events));
    assert_eq!(todo_lists(&first).len(), 2);
    let message = json!({
        "id": "msg_18fa0106b001eWObIIGakHcjOm", "type": "assistant",
        "content": [{"type": "tool", "id": "call_0_1793410994313", "name": "todowrite",
            "state": {"status": "completed", "input": {},
                "metadata": {"todos": [{"content": "first", "status": "pending"}]}}}]});
    let again = flat_events(translator.replay(&[message], false, true));
    assert!(todo_lists(&again).is_empty(), "{again:?}");
}

#[test]
fn list_lost_in_a_gap_comes_from_the_projection() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    let message = json!({
        "id": "msg_1", "type": "assistant",
        "content": [{"type": "tool", "id": "call_1", "name": "todowrite",
            "state": {"status": "completed", "input": {},
                "metadata": {"todos": [{"content": "пункт", "status": "completed"}]}}}]});
    let out = flat_events(translator.replay(&[message], false, true));
    assert_eq!(
        todo_lists(&out),
        vec![(None, vec![todo("пункт", TodoStatus::Completed)])]
    );
}

#[test]
fn failed_todo_call_and_other_tools_change_nothing() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    let events = vec![
        json!({"type": "session.tool.input.started", "data": {
            "sessionID": "ses_root", "assistantMessageID": "msg_1", "id": "call_1",
            "name": "todowrite"}}),
        json!({"type": "session.tool.failed", "data": {
            "sessionID": "ses_root", "assistantMessageID": "msg_1", "id": "call_1",
            "error": {"type": "tool.execution", "message": "нет"},
            "metadata": {"todos": [{"content": "x", "status": "pending"}]}}}),
        json!({"type": "session.tool.input.started", "data": {
            "sessionID": "ses_root", "assistantMessageID": "msg_1", "id": "call_2",
            "name": "shell"}}),
        json!({"type": "session.tool.success", "data": {
            "sessionID": "ses_root", "assistantMessageID": "msg_1", "id": "call_2", "content": [],
            "metadata": {"todos": [{"content": "x", "status": "pending"}]}}}),
    ];
    let out = flat_events(feed_all(&mut translator, &events));
    assert!(todo_lists(&out).is_empty());
}

#[test]
fn unknown_todo_status_is_pending_and_an_item_without_text_is_skipped() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    let events = vec![
        json!({"type": "session.tool.input.started", "data": {
            "sessionID": "ses_root", "assistantMessageID": "msg_1", "id": "call_1",
            "name": "todowrite"}}),
        json!({"type": "session.tool.success", "data": {
            "sessionID": "ses_root", "assistantMessageID": "msg_1", "id": "call_1", "content": [],
            "metadata": {"todos": [
                {"content": "новый статус", "status": "blocked"}, {"status": "pending"}]}}}),
    ];
    let out = flat_events(feed_all(&mut translator, &events));
    assert_eq!(
        todo_lists(&out),
        vec![(None, vec![todo("новый статус", TodoStatus::Pending)])]
    );
}

fn outcomes(translated: &[Translated]) -> Vec<&RunOutcome> {
    translated
        .iter()
        .filter_map(|item| match item {
            Translated::RunEnded(outcome) => Some(outcome),
            _ => None,
        })
        .collect()
}

#[test]
fn scheduled_retry_shows_the_attempt_and_ends_no_turn() {
    let events = stream("service_failure");
    let (mut translator, _) = translator(&events);
    let retries: Vec<Value> = events
        .iter()
        .filter(|event| event["type"] == "session.retry.scheduled")
        .cloned()
        .collect();
    assert!(!retries.is_empty());
    let out = feed_all(&mut translator, &retries);
    assert!(outcomes(&out).is_empty());
    let summaries: Vec<_> = adapter_events(out)
        .into_iter()
        .filter_map(|event| match event {
            AdapterEvent::Phase {
                phase: Phase::Generating,
                summary,
                ..
            } => summary,
            _ => None,
        })
        .collect();
    assert_eq!(summaries.len(), retries.len());
    assert_eq!(summaries[0], "retry 2");
    assert!(summaries.iter().all(|text| !text.contains("slow down")));
}

#[test]
fn retry_of_a_child_is_not_a_phase_of_the_main_agent() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    translator.add_child("ses_a".to_owned());
    let out = translator.feed(&json!({"type": "session.retry.scheduled", "data": {
        "sessionID": "ses_a", "assistantMessageID": "msg_1", "attempt": 2, "at": 1,
        "error": {"type": "provider.internal", "message": "boom"}}}));
    assert!(out.is_empty());
}

#[test]
fn failure_keeps_its_kind_and_the_wait_of_the_provider() {
    let events = capture_events("real_free_model", None);
    let mut translator = Translator::new(&session_of(&events), HashMap::new());
    let out = feed_all(&mut translator, &events);
    let RunOutcome::Failed(failure) = outcomes(&out)[0] else {
        panic!("failed run");
    };
    assert_eq!(failure.kind, "provider.free-limit");
    assert_eq!(failure.retry_after_ms, Some(80_814_000));
    assert!(!failure.overflow);
}

#[test]
fn quota_without_a_wait_has_no_reset() {
    let events = stream("service_failure");
    let (mut translator, _) = translator(&events);
    let out = feed_all(&mut translator, &events);
    let failures: Vec<_> = outcomes(&out)
        .into_iter()
        .filter_map(|outcome| match outcome {
            RunOutcome::Failed(failure) if failure.kind == "provider.quota" => Some(failure),
            _ => None,
        })
        .collect();
    assert_eq!(failures[0].retry_after_ms, Some(3_600_000));
    assert_eq!(failures[1].retry_after_ms, None);
}

// Придуманный кадр: `execution.failed` с переполнением. Форма та же, что у записанного
// `provider.invalid-request`, текст взят из шаблонов переполнения OpenCode 2.1.4.
fn failed_with(message: &str, body: &str) -> Value {
    json!({"type": "session.execution.failed", "data": {
        "sessionID": "ses_root",
        "error": {"type": "provider.invalid-request", "message": message, "status": 400,
                  "response": {"body": body}}}})
}

#[test]
fn overflow_text_is_recognised_in_the_message_and_in_the_body() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    let overflow = |out: Vec<Translated>| match outcomes(&out)[0] {
        RunOutcome::Failed(failure) => failure.overflow,
        other => panic!("{other:?}"),
    };
    assert!(overflow(
        translator.feed(&failed_with("prompt is too long: 900000 tokens", ""))
    ));
    assert!(overflow(translator.feed(&failed_with(
        "bad",
        r#"{"code":"context_length_exceeded"}"#
    ))));
    let invalid = "The request contains invalid parameters. Check the request body for any errors or inconsistencies.";
    assert!(!overflow(translator.feed(&failed_with(invalid, ""))));
}

#[test]
fn compaction_is_context_maintenance_and_its_tokens_are_counted() {
    let events = capture_events("compaction", Some("overflow"));
    let (mut translator, command) = translator(&events);
    let tail: Vec<Value> = events
        .iter()
        .skip_while(|event| event["type"] != "session.compaction.started")
        .take_while(|event| event["type"] != "session.step.started")
        .cloned()
        .collect();
    let out = feed_all(&mut translator, &tail);
    assert!(
        out.iter()
            .any(|item| matches!(item, Translated::CompactionEnded { failed: None, .. }))
    );
    let phases: Vec<_> = adapter_events(out)
        .into_iter()
        .filter_map(|event| match event {
            AdapterEvent::Phase { phase, .. } => Some(format!("{phase:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(phases, ["ContextMaintenance", "Generating"]);
    let AdapterEvent::RunFinished { usage, .. } = translator.finish_run(command) else {
        panic!("run must finish");
    };
    assert_eq!(usage[0].input_tokens, 70);
    assert_eq!(usage[0].agent, "orchestrator");
}

// Форма кадра: `live/compaction.jsonl`, `live/invalid_request_stuck.jsonl`.
fn compaction_failed(error_type: &str, message: &str) -> Value {
    json!({"type": "session.compaction.failed", "data": {
        "sessionID": "ses_root", "reason": "manual", "inputID": "msg_1",
        "error": {"type": error_type, "message": message}}})
}

#[test]
fn failed_compaction_names_an_overflow_only_when_the_provider_said_so() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    let plain = translator.feed(&compaction_failed(
        "compaction.unavailable",
        "Nothing to compact yet",
    ));
    assert!(plain.iter().any(|item| matches!(
        item,
        Translated::CompactionEnded {
            failed: Some(RunFailure {
                overflow: false,
                ..
            }),
            manual: true
        }
    )));
    let overflow = translator.feed(&compaction_failed(
        "provider.invalid-request",
        "prompt is too long",
    ));
    assert!(overflow.iter().any(|item| matches!(
        item,
        Translated::CompactionEnded {
            failed: Some(RunFailure { overflow: true, .. }),
            ..
        }
    )));
}

// `live/location_shutdown.jsonl`: выгрузка простаивающей локации не событие для владельца.
#[test]
fn location_shutdown_needs_no_diagnostic() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    let frame = json!({"type": "location.shutdown", "location": {"directory": "/w"}, "data": {}});
    assert!(translator.feed(&frame).is_empty());
}

#[test]
fn compaction_of_a_child_is_not_reported() {
    let mut translator = Translator::new("ses_root", HashMap::new());
    translator.add_child("ses_a".to_owned());
    let mut event = compaction_failed("compaction.failed", "x");
    event["data"]["sessionID"] = "ses_a".into();
    assert!(translator.feed(&event).is_empty());
}

#[test]
fn unknown_events_carry_data_names_and_never_values() {
    let secret = "sk-live-SECRET-0123456789";
    let mut translator = Translator::new("ses_a", HashMap::new());
    let event = json!({"type": "session.brand.new", "data": {
        "sessionID": "ses_a", "apiKey": secret, "nested": {"password": secret}, "list": [secret],
    }});
    let out = translator.feed(&event);
    let [
        Translated::Event(AdapterEvent::Diagnostic {
            kind: "unknown_event",
            event_type,
            required: false,
            field_names,
        }),
    ] = out.as_slice()
    else {
        panic!("{out:?}");
    };
    assert_eq!(event_type, "session.brand.new");
    assert_eq!(field_names, &["apiKey", "list", "nested", "sessionID"]);
    assert!(!format!("{event_type} {field_names:?}").contains(secret));

    let terminal = json!({"type": "session.execution.paused", "data": {
        "sessionID": "ses_a", "reason": secret,
    }});
    let out = translator.feed(&terminal);
    let [
        Translated::Event(AdapterEvent::Diagnostic {
            required: true,
            field_names,
            ..
        }),
    ] = out.as_slice()
    else {
        panic!("{out:?}");
    };
    assert_eq!(field_names, &["reason", "sessionID"]);

    let typeless = json!({"data": {"sessionID": "ses_a"}, "token": secret});
    let out = translator.feed(&typeless);
    let [
        Translated::Event(AdapterEvent::Diagnostic {
            kind: "unparsed_event",
            field_names,
            ..
        }),
    ] = out.as_slice()
    else {
        panic!("{out:?}");
    };
    assert_eq!(field_names, &["data", "token"]);
}

#[test]
fn compaction_frames_open_and_close_a_timeline_episode() {
    let events = capture_events("compaction", Some("overflow"));
    let (mut translator, _) = translator(&events);
    let tail: Vec<Value> = events
        .iter()
        .skip_while(|event| event["type"] != "session.compaction.started")
        .take_while(|event| event["type"] != "session.step.started")
        .cloned()
        .collect();
    let out = adapter_events(feed_all(&mut translator, &tail));
    // Записанное автоматическое сжатие: эпизод авто, цифры CLI не даёт.
    assert!(
        out.iter().any(|e| matches!(
            e,
            AdapterEvent::CompactionStarted { manual: false }
        )),
        "{out:?}"
    );
    assert!(
        out.iter().any(|e| matches!(
            e,
            AdapterEvent::CompactionFinished {
                manual: false,
                ok: true,
                pre_tokens: 0,
            }
        )),
        "{out:?}"
    );
}

#[test]
fn compaction_failure_closes_the_episode_as_failed() {
    let events = capture_events("compaction", Some("overflow"));
    let session = session_of(&events);
    let (mut translator, _) = translator(&events);
    let failed = json!({"type": "session.compaction.failed", "data": {
        "sessionID": session,
        "reason": "auto",
        "error": {"type": "compaction.unavailable", "message": "Nothing to compact yet"}}});
    let out = adapter_events(translator.feed(&failed));
    assert!(
        out.iter().any(|e| matches!(
            e,
            AdapterEvent::CompactionFinished {
                manual: false,
                ok: false,
                pre_tokens: 0,
            }
        )),
        "{out:?}"
    );
}
