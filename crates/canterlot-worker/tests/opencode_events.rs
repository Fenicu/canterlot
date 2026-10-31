#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Перевод событий OpenCode (L0) на записях этапа 0.

use canterlot_worker::adapter::{
    AdapterEvent, BlockKind, ModelUsage, Phase,
    opencode::{
        catalog,
        events::{RunOutcome, Translated, Translator, block_id},
        replay::capture_lines,
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
    translator.begin_run(command);
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
    translator.begin_run(first);
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
    translator.begin_run(second);
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
    translator.begin_run(Uuid::now_v7());
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
    translator.begin_run(Uuid::now_v7());
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
            Translated::InboxSeen(id) => Some(id),
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
        translator.begin_run(Uuid::now_v7());
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
    translator.begin_run(Uuid::now_v7());
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
