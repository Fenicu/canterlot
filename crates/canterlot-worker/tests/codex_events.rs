//! Разбор событий Codex (задача 2 плана 2b, шаг 1). Записанные кадры —
//! docs/stage0/evidence/codex/, сконструированные — tests/fixtures/codex/.

use canterlot_worker::adapter::{
    AdapterEvent, BlockKind, MediaSource, Phase, TodoItem, TodoStatus,
    codex::events::{Translated, Translator, TurnStatus},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "support/fixtures.rs"]
#[allow(dead_code)] // Цель берёт только codex-повтор из общего файла.
mod fixtures;

use fixtures::codex::{self, Frame};

fn command_id() -> Uuid {
    Uuid::parse_str("f9ec973c-53f4-4369-9109-786f38fcb361").unwrap()
}

// Уведомления (`out` с `method`, включая запросы сервера) в переводчик;
// ответы, входящие, `err`, `Omitted` и `note` пропускаются.
fn feed_frames(translator: &mut Translator, frames: &[Frame]) -> Vec<Translated> {
    let mut out = Vec::new();
    for frame in frames {
        if let Frame::Out(data) = frame
            && let Some(method) = data.get("method").and_then(Value::as_str)
        {
            let params = data.get("params").cloned().unwrap_or(Value::Null);
            out.extend(translator.feed(method, &params));
        }
    }
    out
}

fn feed_line(
    translator: &mut Translator,
    name: &str,
    line: usize,
    method: &str,
) -> Vec<Translated> {
    let data = codex::replay_lines(name)
        .into_iter()
        .find_map(|(n, frame)| match frame {
            Frame::Out(data) if n == line => Some(data),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{name}:{line} is not an out frame"));
    assert_eq!(data["method"], method);
    translator.feed(method, &data.get("params").cloned().unwrap_or(Value::Null))
}

fn events_of(all: &[Translated]) -> Vec<&AdapterEvent> {
    all.iter()
        .filter_map(|t| match t {
            Translated::Event(event) => Some(event),
            _ => None,
        })
        .collect()
}

#[test]
fn agent_message_deltas_join_and_completed_text_is_final() {
    let frames = codex::replay("start_stream");
    let mut translator = Translator::new("01a171c0-2add-72c0-934b-89a64fa8f188", "gpt-6-luna");
    translator.begin_turn("01a171c0-2b09-7951-a207-05d1f9128391", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let started = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Text,
                ..
            } => Some(*block_id),
            _ => None,
        })
        .expect("text block");
    let joined: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockDelta { block_id, append } if *block_id == started => {
                Some(append.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(joined, "PONG");
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { block_id, text, .. }
        if *block_id == started && text == "PONG"
    )));
    // 28 дельт второго захвата тоже дают финальный текст.
    let frames = codex::replay("question");
    let mut translator = Translator::new("01a171c1-daf4-7b03-9198-680fc7eb0395", "gpt-6-luna");
    translator.begin_turn("01a171c1-ddc1-7323-a29b-42ef20d9a74b", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let started = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Text,
                ..
            } => Some(*block_id),
            _ => None,
        })
        .expect("text block");
    let joined: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockDelta { block_id, append } if *block_id == started => {
                Some(append.as_str())
            }
            _ => None,
        })
        .collect();
    let final_text = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockFinal { block_id, text, .. } if *block_id == started => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("final");
    assert_eq!(joined, final_text);
    assert!(!joined.is_empty());
}

#[test]
fn command_items_become_call_and_result_blocks() {
    let frames = codex::replay("tools");
    let mut translator = Translator::new("01a171c0-fdcd-7db1-91ca-9804eb1229dd", "gpt-6-luna");
    translator.begin_turn("01a171c0-fdf7-7b12-a07f-38fb50befd0d", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let calls: Vec<(Uuid, Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name,
                summary,
                ..
            } => Some((*block_id, tool_name.clone(), summary.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|(_, name, _)| name.as_deref() == Some("shell"))
    );
    assert_eq!(calls[0].2.as_deref(), Some("echo canterlot-tool-ok"));
    assert_eq!(calls[1].2.as_deref(), Some("ls /nonexistent-canterlot-dir"));
    let results: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockFinal { block_id, text, .. }
                if events.iter().any(|s| {
                    matches!(
                        s,
                        AdapterEvent::BlockStarted {
                            block_id: parent,
                            kind: BlockKind::ToolResult,
                            ..
                        } if *parent == *block_id
                    )
                }) =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], "canterlot-tool-ok\n");
    assert!(results[1].contains("No such file or directory"));
    let finals: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockFinal {
                block_id, summary, ..
            } if calls.iter().any(|(id, _, _)| id == block_id) => {
                Some(summary.as_deref().unwrap_or(""))
            }
            _ => None,
        })
        .collect();
    assert!(finals[0].contains("exit 0"), "{finals:?}");
    assert!(finals[1].contains("exit 2"), "{finals:?}");
    // Упавший инструмент — обычный результат, ход продолжается текстом "0 2".
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { text, .. } if text == "0 2"
    )));
    // Подтверждения доставки нет: входной userMessage блока не даёт.
    assert!(!events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { text, .. } if text.contains("canterlot-tool-ok") && text.contains("separate tool calls")
    )));
    let proofs: Vec<&Translated> = out
        .iter()
        .filter(|t| matches!(t, Translated::UserMessage { .. }))
        .collect();
    assert_eq!(proofs.len(), 1);
}

#[test]
fn file_change_item_carries_diff_and_declined_status() {
    let frames = codex::replay("permissions");
    let mut translator = Translator::new("01a171c1-2578-7941-bc6d-d6ddc5b5ca1f", "gpt-6-luna");
    translator.set_workdir(std::path::Path::new(
        "/tmp/canterlot-stage0/codex/permissions/run-1792909845506",
    ));
    translator.begin_turn("01a171c1-259e-7d31-a49c-500a44d9eed4", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let call = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name,
                summary,
                ..
            } if tool_name.as_deref() == Some("apply_patch") => Some((*block_id, summary.clone())),
            _ => None,
        })
        .expect("apply_patch block");
    assert_eq!(call.1.as_deref(), Some("note.txt"));
    let final_text = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockFinal { block_id, text, .. } if *block_id == call.0 => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("apply_patch final");
    assert_eq!(final_text, "hi\n");
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { block_id, summary, .. }
        if *block_id == call.0 && summary.as_deref().is_some_and(|s| s.contains("declined"))
    )));
    // Оба запроса разрешений прошли через переводчик: метка CLI не ставится.
    let completed: Vec<(&str, &str, bool)> = out
        .iter()
        .filter_map(|t| match t {
            Translated::ItemCompleted {
                item_id,
                item_type: _,
                status,
                approved_by_cli,
            } => Some((
                item_id.as_str(),
                status.as_deref().unwrap_or(""),
                *approved_by_cli,
            )),
            _ => None,
        })
        .collect();
    assert!(
        completed
            .iter()
            .any(|(id, _, approved)| id.starts_with("exec-") && !approved)
    );
    assert!(completed.iter().any(|(_, _, _)| {
        completed
            .iter()
            .any(|(id, status, _)| id.starts_with("exec-579f86a8") && *status == "declined")
    }));
}

#[test]
fn late_completion_of_an_earlier_turn_is_dropped() {
    let mut translator = Translator::new("01a171c6-3ae7-7a63-8b6f-428ed24276d2", "gpt-6-luna");
    translator.begin_turn("01a171c6-3ece-7bc1-9ce0-38db51bc35cc", command_id());
    // ev/interrupt.jsonl:26 — команда sleep открывается…
    let out26 = feed_line(&mut translator, "interrupt", 26, "item/started");
    assert!(events_of(&out26).into_iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::ToolCall,
            ..
        }
    )));
    // …:34 — ход прерван и забыт…
    let out34 = feed_line(&mut translator, "interrupt", 34, "turn/completed");
    assert!(out34.iter().any(|t| matches!(
        t,
        Translated::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        }
    )));
    // …:41 — позднее завершение команды старого хода ничего не даёт.
    let out41 = feed_line(&mut translator, "interrupt", 41, "item/completed");
    assert!(
        events_of(&out41).is_empty(),
        "late completion leaked: {out41:?}"
    );
}

#[test]
fn usage_is_sum_of_last_per_turn_and_matches_total() {
    let frames = codex::replay("tools");
    let mut translator = Translator::new("01a171c0-fdcd-7db1-91ca-9804eb1229dd", "gpt-6-luna");
    translator.begin_turn("01a171c0-fdf7-7b12-a07f-38fb50befd0d", command_id());
    feed_frames(&mut translator, &frames);
    match translator.finish_turn() {
        AdapterEvent::RunFinished {
            usage,
            context_tokens,
            context_window_tokens,
            ..
        } => {
            assert_eq!(usage.len(), 1);
            let u = &usage[0];
            assert_eq!(u.model, "gpt-6-luna");
            assert_eq!(u.agent, "orchestrator");
            assert_eq!(u.input_tokens, 31964 - 30336);
            assert_eq!(u.output_tokens, 45 + 47 + 7);
            assert_eq!(u.cache_read_tokens, 30336);
            assert_eq!(u.cache_write_tokens, 0);
            assert_eq!(
                u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens,
                32063
            );
            assert_eq!(context_tokens, 10777);
            assert_eq!(context_window_tokens, 258400);
        }
        other => panic!("expected RunFinished, got {other:?}"),
    }
}

#[test]
fn usage_replayed_after_resume_is_not_counted() {
    let mut translator = Translator::new("01a171c3-4681-7b81-bd8b-be81cac722c8", "gpt-6-luna");
    translator.begin_turn("01a171c3-54f3-76e1-83b3-3568b95f3060", command_id());
    // ev/resume.jsonl:52 — повтор старого хода после thread/resume: мимо.
    let out52 = feed_line(&mut translator, "resume", 52, "thread/tokenUsage/updated");
    assert!(out52.iter().all(|t| matches!(t, Translated::Ignored)));
    // :105 — обновление нового хода: считается.
    feed_line(&mut translator, "resume", 105, "thread/tokenUsage/updated");
    match translator.finish_turn() {
        AdapterEvent::RunFinished {
            usage,
            context_tokens,
            context_window_tokens,
            ..
        } => {
            assert_eq!(usage.len(), 1);
            let u = &usage[0];
            assert_eq!(u.input_tokens, 10539 - 10368);
            assert_eq!(u.output_tokens, 9);
            assert_eq!(u.cache_read_tokens, 10368);
            assert_eq!(context_tokens, 10548);
            assert_eq!(context_window_tokens, 258400);
        }
        other => panic!("expected RunFinished, got {other:?}"),
    }
}

#[test]
fn context_tokens_and_window_from_last_update() {
    let frames = codex::replay("tools");
    let mut translator = Translator::new("01a171c0-fdcd-7db1-91ca-9804eb1229dd", "gpt-6-luna");
    translator.begin_turn("01a171c0-fdf7-7b12-a07f-38fb50befd0d", command_id());
    feed_frames(&mut translator, &frames);
    match translator.finish_turn() {
        AdapterEvent::RunFinished {
            context_tokens,
            context_window_tokens,
            ..
        } => {
            assert_eq!(context_tokens, 10777);
            assert_eq!(context_window_tokens, 258400);
        }
        other => panic!("expected RunFinished, got {other:?}"),
    }
}

fn is_phase(event: &AdapterEvent, phase: &Phase) -> bool {
    matches!(event, AdapterEvent::Phase { phase: p, .. } if std::mem::discriminant(p) == std::mem::discriminant(phase))
}

#[test]
fn phases_follow_status_flags_and_awaiting_input_waits_for_turn_end() {
    let mut translator = Translator::new("01a171c1-2578-7941-bc6d-d6ddc5b5ca1f", "gpt-6-luna");
    translator.begin_turn("01a171c1-259e-7d31-a49c-500a44d9eed4", command_id());
    let out25 = feed_line(&mut translator, "permissions", 25, "thread/status/changed");
    assert!(
        events_of(&out25)
            .into_iter()
            .any(|e| is_phase(e, &Phase::AwaitingPermission))
    );
    let out30 = feed_line(&mut translator, "permissions", 30, "thread/status/changed");
    assert!(
        events_of(&out30)
            .into_iter()
            .any(|e| is_phase(e, &Phase::Generating))
    );
    let mut translator = Translator::new("01a171cb-3faf-7f01-996d-a42835bb3524", "gpt-6-luna");
    translator.begin_turn("01a171cb-4445-7d13-8e41-184bfa519a0b", command_id());
    let out27 = feed_line(
        &mut translator,
        "question_default_mode",
        27,
        "thread/status/changed",
    );
    assert!(
        events_of(&out27)
            .into_iter()
            .any(|e| is_phase(e, &Phase::AwaitingAnswer))
    );
    let out31 = feed_line(
        &mut translator,
        "question_default_mode",
        31,
        "thread/status/changed",
    );
    assert!(
        events_of(&out31)
            .into_iter()
            .any(|e| is_phase(e, &Phase::Generating))
    );
    // idle приходит раньше turn/completed: AwaitingInput не издаём.
    let frames = codex::replay("start_stream");
    let mut translator = Translator::new("01a171c0-2add-72c0-934b-89a64fa8f188", "gpt-6-luna");
    translator.begin_turn("01a171c0-2b09-7951-a207-05d1f9128391", command_id());
    let out = feed_frames(&mut translator, &frames);
    assert!(
        !events_of(&out)
            .into_iter()
            .any(|e| is_phase(e, &Phase::AwaitingInput))
    );
    assert!(out.iter().any(|t| matches!(
        t,
        Translated::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        }
    )));
}

#[test]
fn retrying_errors_become_phase_summaries_and_final_error_is_kept() {
    let frames = codex::replay("service_failure");
    // В записи два запуска: повторы в первом треде, отказ модели во втором.
    let mut retrying = Translator::new("01a171c3-ac8d-7682-9fc0-125ff2c04433", "gpt-6-luna");
    retrying.begin_turn("01a171c3-b957-78d3-85c8-1af41fdc61fd", command_id());
    let retried = feed_frames(&mut retrying, &frames);
    let mut translator = Translator::new("01a171c6-1373-7730-9f41-665a0cd58fb2", "gpt-6-luna");
    translator.begin_turn("01a171c6-16c1-7103-9495-c288ebdf54e3", command_id());
    let out = feed_frames(&mut translator, &frames);
    let summaries: Vec<String> = events_of(&retried)
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Phase {
                phase: Phase::Generating,
                summary: Some(summary),
                ..
            } => Some(summary.clone()),
            _ => None,
        })
        .collect();
    let retries: Vec<&String> = summaries.iter().filter(|s| s.contains("повтор")).collect();
    assert_eq!(retries.len(), 6);
    for (i, summary) in retries.iter().enumerate() {
        assert!(summary.contains(&format!("({})", i + 1)), "{summary}");
    }
    assert!(out.iter().any(|t| matches!(
        t,
        Translated::TurnCompleted {
            status: TurnStatus::Failed,
            error: Some(_),
            ..
        }
    )));
    let error = out
        .iter()
        .find_map(|t| match t {
            Translated::TurnCompleted { error: Some(e), .. } => Some(e),
            _ => None,
        })
        .expect("turn error");
    assert_eq!(error.kind, "other");
}

#[test]
fn warning_becomes_warning_block_and_feature_warning_is_dropped() {
    let mut translator = Translator::new("01a171c6-1373-7730-9f41-665a0cd58fb2", "gpt-6-luna");
    translator.begin_turn("01a171c6-16c1-7103-9495-c288ebdf54e3", command_id());
    let out62 = feed_line(&mut translator, "service_failure", 62, "warning");
    let events = events_of(&out62);
    let block = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Warning,
                input_command_id,
                ..
            } => Some((*block_id, *input_command_id)),
            _ => None,
        })
        .expect("warning block");
    assert_eq!(block.1, Some(command_id()));
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { block_id, text, .. }
        if *block_id == block.0 && text.contains("canterlot-no-such-model")
    )));
    // Вне хода — необязательная диагностика.
    let mut translator = Translator::new("01a171c6-1373-7730-9f41-665a0cd58fb2", "gpt-6-luna");
    let out = feed_line(&mut translator, "service_failure", 62, "warning");
    assert!(events_of(&out).into_iter().any(|e| matches!(
        e,
        AdapterEvent::Diagnostic {
            required: false,
            ..
        }
    )));
    // Штатное предупреждение о флаге давится.
    let mut translator = Translator::new("01a171cb-3faf-7f01-996d-a42835bb3524", "gpt-6-luna");
    translator.begin_turn("turn-x", command_id());
    let out20 = feed_line(&mut translator, "question_default_mode", 20, "warning");
    assert!(events_of(&out20).is_empty(), "{out20:?}");
}

#[test]
fn unknown_and_noise_notifications_never_end_the_stream() {
    let mut translator = Translator::new("01a171c1-daf4-7b03-9198-680fc7eb0395", "gpt-6-luna");
    translator.begin_turn("01a171c1-ddc1-7323-a29b-42ef20d9a74b", command_id());
    for (index, method) in [
        (103, "turn/moderationMetadata"),
        (112, "thread/readState/changed"),
    ] {
        let out = feed_line(&mut translator, "question", index, method);
        assert!(
            out.iter().all(|t| matches!(t, Translated::Ignored)),
            "{out:?}"
        );
    }
    let out7 = feed_line(
        &mut translator,
        "start_stream",
        7,
        "remoteControl/status/changed",
    );
    assert!(out7.iter().all(|t| matches!(t, Translated::Ignored)));
    // Выдуманный метод — Ignored плюс одна диагностика на метод за исполнение.
    let first = translator.feed("turn/frobnicate", &Value::Null);
    assert!(
        first
            .iter()
            .any(|t| matches!(t, Translated::Event(AdapterEvent::Diagnostic { .. })))
    );
    let second = translator.feed("turn/frobnicate", &Value::Null);
    assert!(
        !second
            .iter()
            .any(|t| matches!(t, Translated::Event(AdapterEvent::Diagnostic { .. })))
    );
    // Неизвестный тип элемента — именованный ToolCall-блок.
    let out = translator.feed(
        "item/started",
        &serde_json::json!({"threadId": "01a171c1-daf4-7b03-9198-680fc7eb0395", "turnId": "01a171c1-ddc1-7323-a29b-42ef20d9a74b", "item": {"id": "x1", "type": "hookPrompt"}}),
    );
    assert!(events_of(&out).into_iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted { tool_name: Some(name), .. } if name == "hookPrompt"
    )));
    // Не-JSON строка stdout — диагностика, поток жив.
    match translator.feed_unparsed() {
        Translated::Event(AdapterEvent::Diagnostic { .. }) => {}
        other => panic!("expected diagnostic, got {other:?}"),
    }
    let after = translator.feed("turn/moderationMetadata", &Value::Null);
    assert!(after.iter().all(|t| matches!(t, Translated::Ignored)));
}

#[test]
fn subagent_items_carry_agent_model_and_spawn_parent() {
    let frames = codex::replay("subagent");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let spawn = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name,
                summary,
                model,
                ..
            } if tool_name.as_deref() == Some("spawn_agent") => {
                Some((*block_id, summary.clone(), model.clone()))
            }
            _ => None,
        })
        .expect("spawn block");
    assert_eq!(spawn.1.as_deref(), Some("Check the tests"));
    assert_eq!(spawn.2, "gpt-6-mini");
    let child_text = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Text,
                agent,
                model,
                parent,
                ..
            } if agent == "helper" => Some((*block_id, model.clone(), *parent)),
            _ => None,
        })
        .expect("child text block");
    assert_eq!(child_text.1, "gpt-6-mini");
    assert_eq!(child_text.2, Some(spawn.0));
    // Конец детского хода главный не завершает: ровно один TurnCompleted.
    assert_eq!(
        out.iter()
            .filter(|t| matches!(t, Translated::TurnCompleted { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::Phase { summary: Some(s), .. } if s == "Субагент helper: working"
    )));
    match translator.finish_turn() {
        AdapterEvent::RunFinished { usage, .. } => {
            assert_eq!(usage.len(), 2);
            let helper = usage
                .iter()
                .find(|u| u.agent == "helper")
                .expect("helper usage");
            assert_eq!(helper.model, "gpt-6-mini");
            assert_eq!(helper.input_tokens, 90);
            assert_eq!(helper.output_tokens, 5);
            assert_eq!(helper.cache_read_tokens, 10);
            assert_eq!(helper.cache_write_tokens, 2);
            let main = usage
                .iter()
                .find(|u| u.agent == "orchestrator")
                .expect("main usage");
            assert_eq!(main.input_tokens, 180);
            assert_eq!(main.output_tokens, 7);
        }
        other => panic!("expected RunFinished, got {other:?}"),
    }
}

#[test]
fn reasoning_summary_parts_become_one_reasoning_block() {
    let frames = codex::replay("reasoning");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let started: Vec<Uuid> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Reasoning,
                ..
            } => Some(*block_id),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1);
    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockDelta { block_id, append } if *block_id == started[0] => {
                Some(append.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "First\n\nSecond");
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { block_id, text, .. }
        if *block_id == started[0] && text == "First\n\nSecond"
    )));
    // Пустой элемент r2 ничего не дал: финалов ровно один.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::BlockFinal { .. }))
            .count(),
        1
    );
}

#[test]
fn image_view_publishes_media_from_path_once() {
    let frames = codex::replay("image_view");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let block = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name,
                summary,
                ..
            } if tool_name.as_deref() == Some("view_image") => Some((*block_id, summary.clone())),
            _ => None,
        })
        .expect("view_image block");
    assert_eq!(block.1.as_deref(), Some("pic.png"));
    let media: Vec<&&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::Media { .. }))
        .collect();
    assert_eq!(media.len(), 1);
    match media[0] {
        AdapterEvent::Media { block_id, source } => {
            assert_eq!(*block_id, block.0);
            match source {
                MediaSource::Path(path) => assert_eq!(path.to_str().unwrap(), "/tmp/pic.png"),
                other => panic!("expected path, got {other:?}"),
            }
        }
        _ => unreachable!(),
    }
}

#[test]
fn mcp_tool_call_and_output_delta_blocks() {
    let frames = codex::replay("mcp_tool");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let call = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name,
                summary,
                ..
            } if tool_name.as_deref() == Some("s.t") => Some((*block_id, summary.clone())),
            _ => None,
        })
        .expect("mcp block");
    assert_eq!(call.1.as_deref(), Some("hello"));
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::Phase {
            phase: Phase::RunningTool,
            summary: Some(s),
            ..
        } if s == "calling"
    )));
    let result_text = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockFinal { block_id, text, .. }
                if events.iter().any(|s| {
                    matches!(
                        s,
                        AdapterEvent::BlockStarted {
                            block_id: parent,
                            kind: BlockKind::ToolResult,
                            ..
                        } if *parent == *block_id
                    )
                }) =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("mcp result");
    assert_eq!(result_text, "out");
    // Дельты вывода открывают ToolResult, финал — из aggregatedOutput.
    let frames = codex::replay("output_delta");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    let events = events_of(&out);
    let joined: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockDelta { append, .. } => Some(append.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(joined, "tick tock");
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { text, summary, .. }
        if text == "tick tock" && summary.is_none()
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { text, summary, .. }
        if text.contains("sleep 1") && summary.as_deref().is_some_and(|s| s.contains("exit 0"))
    )));
}

#[test]
fn approved_command_leaves_the_waiting_phase() {
    // Запись: флаг ожидания ставится раньше item/started, снимается уже при
    // запущенной команде (ev/permissions.jsonl:25-30).
    let mut translator = Translator::new("01a171c1-2578-7941-bc6d-d6ddc5b5ca1f", "gpt-6-luna");
    translator.begin_turn("01a171c1-259e-7d31-a49c-500a44d9eed4", command_id());
    feed_line(&mut translator, "permissions", 25, "thread/status/changed");
    feed_line(&mut translator, "permissions", 26, "item/started");
    let out = feed_line(&mut translator, "permissions", 30, "thread/status/changed");
    assert!(
        events_of(&out).into_iter().any(|e| matches!(
            e,
            AdapterEvent::Phase { phase: Phase::RunningTool, tool_name: Some(name), .. }
                if name == "shell"
        )),
        "{out:?}"
    );
}

#[test]
fn compaction_end_returns_to_generating() {
    let frames = codex::replay("compaction");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    let phases: Vec<&AdapterEvent> = events_of(&out)
        .into_iter()
        .filter(|e| matches!(e, AdapterEvent::Phase { .. }))
        .collect();
    assert!(
        phases
            .last()
            .is_some_and(|e| is_phase(e, &Phase::Generating)),
        "{phases:?}"
    );
    // Устаревшее уведомление приходит уже после сжатия.
    let out = translator.feed(
        "thread/compacted",
        &json!({"threadId": "thread-main", "turnId": "turn-main"}),
    );
    assert!(events_of(&out).is_empty(), "{out:?}");
}

fn spawn_item(status: &str, children: &[&str]) -> Value {
    json!({"threadId": "thread-main", "turnId": "turn-main", "item": {
        "id": "spawn-1", "type": "collabAgentToolCall", "tool": "spawnAgent",
        "status": status, "receiverThreadIds": children, "prompt": "Check"}})
}

fn child_started() -> Value {
    json!({"thread": {"id": "thread-child-1", "parentThreadId": "thread-main",
        "agentRole": "helper", "model": "gpt-6-mini"}})
}

fn child_block_parent(translator: &mut Translator) -> Option<Uuid> {
    let out = translator.feed(
        "item/started",
        &json!({"threadId": "thread-child-1", "turnId": "turn-child", "item": {
            "id": "msg-c1", "type": "agentMessage", "text": ""}}),
    );
    events_of(&out).into_iter().find_map(|e| match e {
        AdapterEvent::BlockStarted { parent, .. } => *parent,
        _ => None,
    })
}

#[test]
fn spawn_parent_comes_from_the_completed_item_in_either_order() {
    for child_first in [true, false] {
        let mut translator = Translator::new("thread-main", "gpt-6-luna");
        translator.begin_turn("turn-main", command_id());
        let out = translator.feed("item/started", &spawn_item("inProgress", &[]));
        let spawn = events_of(&out)
            .into_iter()
            .find_map(|e| match e {
                AdapterEvent::BlockStarted { block_id, .. } => Some(*block_id),
                _ => None,
            })
            .expect("spawn block");
        if child_first {
            translator.feed("thread/started", &child_started());
        }
        translator.feed(
            "item/completed",
            &spawn_item("completed", &["thread-child-1"]),
        );
        if !child_first {
            translator.feed("thread/started", &child_started());
        }
        assert_eq!(
            child_block_parent(&mut translator),
            Some(spawn),
            "child first: {child_first}"
        );
    }
}

#[test]
fn sub_agent_activity_item_is_a_phase_without_a_block() {
    // В схеме 0.164 это элемент ThreadItem, а не уведомление.
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    translator.feed("thread/started", &child_started());
    let item = json!({"threadId": "thread-main", "turnId": "turn-main", "item": {
        "id": "act-1", "type": "subAgentActivity", "agentThreadId": "thread-child-1",
        "agentPath": "/helper", "kind": "interacted"}});
    let out = translator.feed("item/started", &item);
    let events = events_of(&out);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::BlockStarted { .. })),
        "{out:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AdapterEvent::Phase { summary: Some(s), .. } if s == "Субагент helper: interacted"
        )),
        "{out:?}"
    );
    assert!(events_of(&translator.feed("item/completed", &item)).is_empty());
}

#[test]
fn child_thread_reroute_keeps_the_main_model() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    translator.feed(
        "model/rerouted",
        &json!({"threadId": "thread-child", "turnId": "turn-child",
            "fromModel": "gpt-6-luna", "toModel": "gpt-6-terra", "reason": "highRiskCyberActivity"}),
    );
    let out = translator.feed(
        "item/started",
        &json!({"threadId": "thread-main", "turnId": "turn-main",
            "item": {"id": "m-2", "type": "agentMessage", "text": ""}}),
    );
    assert!(
        events_of(&out).into_iter().any(
            |e| matches!(e, AdapterEvent::BlockStarted { model, .. } if model == "gpt-6-luna")
        ),
        "{out:?}"
    );
    translator.feed(
        "model/rerouted",
        &json!({"threadId": "thread-main", "turnId": "turn-main",
            "fromModel": "gpt-6-luna", "toModel": "gpt-6-terra", "reason": "highRiskCyberActivity"}),
    );
    let out = translator.feed(
        "item/started",
        &json!({"threadId": "thread-main", "turnId": "turn-main",
            "item": {"id": "m-3", "type": "agentMessage", "text": ""}}),
    );
    assert!(
        events_of(&out).into_iter().any(
            |e| matches!(e, AdapterEvent::BlockStarted { model, .. } if model == "gpt-6-terra")
        ),
        "{out:?}"
    );
}

#[test]
fn compaction_sets_context_maintenance_and_window_exceeded_is_context_exhausted() {
    let frames = codex::replay("compaction");
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = feed_frames(&mut translator, &frames);
    assert!(
        events_of(&out)
            .into_iter()
            .any(|e| is_phase(e, &Phase::ContextMaintenance))
    );
    let frames = codex::replay("context_exceeded");
    let out = feed_frames(&mut translator, &frames);
    assert!(out.iter().any(|t| matches!(
        t,
        Translated::TurnCompleted {
            status: TurnStatus::Failed,
            error: Some(e),
            ..
        } if e.kind == "contextWindowExceeded"
    )));
    assert!(
        events_of(&out)
            .into_iter()
            .any(|e| matches!(e, AdapterEvent::ContextExhausted))
    );
}

#[test]
fn debug_output_contains_no_text() {
    let holding = Translated::TurnCompleted {
        turn_id: "t".into(),
        status: TurnStatus::Failed,
        error: Some(canterlot_worker::adapter::codex::events::TurnErrorInfo {
            kind: "other".into(),
            http_status: None,
            message: "secret-msg".into(),
        }),
    };
    let debug = format!("{holding:?}");
    assert!(!debug.contains("secret-msg"), "{debug}");
    let holding = Translated::Event(AdapterEvent::BlockFinal {
        block_id: command_id(),
        text: "secret-text".into(),
        summary: Some("secret-summary".into()),
    });
    let debug = format!("{holding:?}");
    assert!(!debug.contains("secret-text"), "{debug}");
    assert!(!debug.contains("secret-summary"), "{debug}");
    let translator = Translator::new("thread-main", "gpt-6-luna");
    let debug = format!("{translator:?}");
    assert!(!debug.is_empty());
}

// Форма по схеме TurnPlanUpdatedNotification 0.164.0; живого кадра ещё нет.
fn plan_update(thread: &str, plan: Value) -> Value {
    json!({"threadId": thread, "turnId": "turn-main", "explanation": null, "plan": plan})
}

fn todo_events(out: &[Translated]) -> Vec<(Option<Uuid>, Vec<TodoItem>)> {
    events_of(out)
        .into_iter()
        .filter_map(|e| match e {
            AdapterEvent::TodoUpdated { lane, items } => Some((*lane, items.clone())),
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

#[test]
fn turn_plan_is_a_full_list_in_the_main_lane() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let plan = json!([
        {"step": "Собрать", "status": "completed"},
        {"step": "Проверить", "status": "inProgress"},
        {"step": "Выкатить", "status": "pending"},
    ]);
    let out = translator.feed("turn/plan/updated", &plan_update("thread-main", plan));
    assert_eq!(
        todo_events(&out),
        vec![(
            None,
            vec![
                todo("Собрать", TodoStatus::Completed),
                todo("Проверить", TodoStatus::InProgress),
                todo("Выкатить", TodoStatus::Pending),
            ]
        )]
    );
}

#[test]
fn child_thread_plan_goes_to_the_lane_of_its_spawn() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = translator.feed("item/started", &spawn_item("inProgress", &[]));
    let spawn = events_of(&out)
        .into_iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted { block_id, .. } => Some(*block_id),
            _ => None,
        })
        .expect("spawn block");
    translator.feed("thread/started", &child_started());
    let plan = json!([{"step": "Читать", "status": "pending"}]);

    // Родитель известен только из завершённого вызова: до него список не приписать никому.
    let early = translator.feed(
        "turn/plan/updated",
        &plan_update("thread-child-1", plan.clone()),
    );
    assert_eq!(todo_events(&early), vec![]);
    assert!(
        events_of(&early).iter().any(|e| matches!(
            e,
            AdapterEvent::Diagnostic {
                kind: "todo_lane_unknown",
                required: false,
                ..
            }
        )),
        "{early:?}"
    );

    translator.feed(
        "item/completed",
        &spawn_item("completed", &["thread-child-1"]),
    );
    let out = translator.feed("turn/plan/updated", &plan_update("thread-child-1", plan));
    assert_eq!(
        todo_events(&out),
        vec![(Some(spawn), vec![todo("Читать", TodoStatus::Pending)])]
    );
}

#[test]
fn plan_of_a_foreign_thread_is_ignored_and_text_stays_out_of_debug() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    let marker = "PRIVATE_PLAN_MARKER";
    let plan = json!([{"step": marker, "status": "pending"}]);
    let out = translator.feed(
        "turn/plan/updated",
        &plan_update("thread-other", plan.clone()),
    );
    assert!(todo_events(&out).is_empty(), "{out:?}");
    assert_eq!(
        diagnostics(&events_of(&out)),
        [(
            "unattributed_thread_frame".to_owned(),
            "turn/plan/updated".to_owned()
        )]
    );
    let out = translator.feed("turn/plan/updated", &plan_update("thread-main", plan));
    assert_eq!(todo_events(&out).len(), 1);
    assert!(!format!("{out:?}").contains(marker), "{out:?}");
}

// `\u0000` в JSON — законная строка: разбор его не снимает, поэтому заменяет его хаб.
#[test]
fn json_nul_reaches_block_text_from_command_output_and_agent_text() {
    let frame = |body: &str| {
        serde_json::from_str::<Value>(&format!(
            r#"{{{body},"threadId":"thread","turnId":"turn"}}"#
        ))
        .unwrap()
    };
    let mut translator = Translator::new("thread", "gpt-6-luna");
    translator.begin_turn("turn", command_id());
    translator.feed(
        "item/started",
        &frame(r#""item":{"id":"c1","type":"commandExecution","command":"cat x","status":"inProgress"}"#),
    );
    let streamed = translator.feed(
        "item/commandExecution/outputDelta",
        &frame(r#""itemId":"c1","delta":"a\u0000b""#),
    );
    assert!(
        events_of(&streamed).iter().any(|event| matches!(
            event,
            AdapterEvent::BlockDelta { append, .. } if append == "a\0b"
        )),
        "{streamed:?}"
    );
    let done = translator.feed(
        "item/completed",
        &frame(
            r#""item":{"id":"c1","type":"commandExecution","command":"cat x","status":"completed","exitCode":0,"aggregatedOutput":"a\u0000b"}"#,
        ),
    );
    assert!(
        events_of(&done).iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { text, .. } if text == "a\0b"
        )),
        "{done:?}"
    );

    translator.feed(
        "item/started",
        &frame(r#""item":{"id":"m1","type":"agentMessage","text":""}"#),
    );
    let text = translator.feed(
        "item/agentMessage/delta",
        &frame(r#""itemId":"m1","delta":"c\u0000d""#),
    );
    assert!(
        events_of(&text).iter().any(|event| matches!(
            event,
            AdapterEvent::BlockDelta { append, .. } if append == "c\0d"
        )),
        "{text:?}"
    );
}

#[test]
fn unknown_notification_carries_param_names_and_never_values() {
    let secret = "sk-live-SECRET-0123456789";
    let mut translator = Translator::new("01a171c1-daf4-7b03-9198-680fc7eb0395", "gpt-6-luna");
    let params = serde_json::json!({
        "threadId": "01a171c1-daf4-7b03-9198-680fc7eb0395", "apiKey": secret,
        "nested": {"password": secret}, "list": [secret],
    });
    let out = translator.feed("thread/futureThing", &params);
    let diagnostics: Vec<&AdapterEvent> = out
        .iter()
        .filter_map(|t| match t {
            Translated::Event(event @ AdapterEvent::Diagnostic { .. }) => Some(event),
            _ => None,
        })
        .collect();
    let [
        AdapterEvent::Diagnostic {
            kind: "unknown_notification",
            event_type,
            required: false,
            field_names,
        },
    ] = diagnostics.as_slice()
    else {
        panic!("{out:?}");
    };
    assert_eq!(event_type, "thread/futureThing");
    assert_eq!(field_names, &["apiKey", "list", "nested", "threadId"]);
    assert!(!format!("{event_type} {field_names:?}").contains(secret));
}

#[test]
fn unknown_notification_names_are_bounded() {
    let mut translator = Translator::new("01a171c1-daf4-7b03-9198-680fc7eb0395", "gpt-6-luna");
    let mut reported = 0;
    for n in 0..200 {
        let out = translator.feed(&format!("future/method{n}"), &Value::Null);
        reported += out
            .iter()
            .filter(|t| matches!(t, Translated::Event(AdapterEvent::Diagnostic { .. })))
            .count();
    }
    assert_eq!(reported, 64);
}

#[test]
fn long_command_label_is_marked_as_cut() {
    let label = |command: String| {
        let mut translator = Translator::new("thread", "gpt-6-luna");
        translator.begin_turn("turn", command_id());
        let out = translator.feed(
            "item/started",
            &json!({"threadId": "thread", "turnId": "turn", "item": {
                "id": "c1", "type": "commandExecution", "command": command, "status": "inProgress"}}),
        );
        events_of(&out)
            .iter()
            .find_map(|event| match event {
                AdapterEvent::BlockStarted {
                    summary: Some(summary),
                    ..
                } => Some(summary.clone()),
                _ => None,
            })
            .expect("the call has a summary")
    };
    assert_eq!(label("a".repeat(120)), "a".repeat(120));
    assert_eq!(label("a".repeat(121)), format!("{}…", "a".repeat(119)));
}

fn diagnostics(events: &[&AdapterEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::Diagnostic {
                kind, event_type, ..
            } => Some(((*kind).to_owned(), event_type.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn async_question_stays_a_text_block_and_raises_a_diagnostic() {
    // Настоящий кадр: request_user_input_async, карточки нет, ничего не ждёт.
    let thread = "01a196a8-4b17-7e32-ab40-54c2d402cc39";
    let turn = "01a196ac-6014-7a50-a2ef-042266c1e3f5";
    let mut translator = Translator::new(thread, "gpt-6-luna");
    translator.begin_turn(turn, command_id());
    let item = "call_4f42fda4cbc7403abdc4a4a5153d5586";
    translator.feed(
        "item/started",
        &json!({"threadId": thread, "turnId": turn, "item": {
            "id": item, "type": "agentMessage", "text": ""}}),
    );
    let out = feed_frames(&mut translator, &codex::replay("async_question_live"));
    let events = events_of(&out);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AdapterEvent::BlockFinal { text, .. } if text.starts_with("Color")
        )),
        "{out:?}"
    );
    assert_eq!(
        diagnostics(&events),
        [(
            "codex_async_question".to_owned(),
            "codex_async_question".to_owned()
        )]
    );
}

// --- субагенты: настоящие кадры 0.164, thread/started потомка не приходит ---

const V2_MAIN: &str = "01a196cc-ca97-7d43-93e6-a7e30f1cf261";
const V2_TURN: &str = "01a196cc-caf5-76c2-b655-aeafe76b84fa";
const V1_MAIN: &str = "01a196cb-4cb9-7f73-96ef-5462ba5d6a1a";
const V1_TURN: &str = "01a196cc-3ec9-7b30-abfe-cbbac2c028aa";
const LATE_MAIN: &str = "01a196cf-4e55-71e0-a23e-aa1bcde289f6";
const LATE_TURN: &str = "01a196cf-4e70-7e82-9f5f-12ad7315cd1e";

struct SpawnCall {
    block: Uuid,
    agent: String,
    summary: Option<String>,
    model: String,
}

fn spawn_calls(events: &[&AdapterEvent]) -> Vec<SpawnCall> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name,
                agent,
                summary,
                model,
                ..
            } if tool_name.as_deref() == Some("spawn_agent") => Some(SpawnCall {
                block: *block_id,
                agent: agent.clone(),
                summary: summary.clone(),
                model: model.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn child_blocks(events: &[&AdapterEvent], call: Uuid) -> Vec<(BlockKind, String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                kind,
                parent: Some(parent),
                agent,
                model,
                ..
            } if *parent == call && agent != "orchestrator" => {
                Some((kind.clone(), agent.clone(), model.clone()))
            }
            _ => None,
        })
        .collect()
}

// Итог потомка: блок результата под вызовом и его финал.
fn spawn_result(events: &[&AdapterEvent], call: Uuid) -> Option<(String, Option<String>, String)> {
    let (block, agent) = events.iter().find_map(|event| match event {
        AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::ToolResult,
            parent: Some(parent),
            agent,
            ..
        } if *parent == call => Some((*block_id, agent.clone())),
        _ => None,
    })?;
    events.iter().find_map(|event| match event {
        AdapterEvent::BlockFinal {
            block_id,
            text,
            summary,
        } if *block_id == block => Some((text.clone(), summary.clone(), agent.clone())),
        _ => None,
    })
}

#[test]
fn subagent_from_activity_item_gets_a_call_a_lane_and_a_result() {
    let mut translator = Translator::new(V2_MAIN, "gpt-6-luna");
    translator.begin_turn(V2_TURN, command_id());
    let out = feed_frames(&mut translator, &codex::replay("subagent_live_v2"));
    let events = events_of(&out);
    let calls = spawn_calls(&events);
    assert_eq!(calls.len(), 1, "{out:?}");
    assert_eq!(calls[0].agent, "orchestrator");
    assert_eq!(
        calls[0].summary, None,
        "the task text is encrypted in 0.164"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockFinal { block_id, text, .. }
            if *block_id == calls[0].block && text == "/root/lister"
    )));
    let lane = child_blocks(&events, calls[0].block);
    assert!(
        lane.iter()
            .all(|(_, agent, model)| agent == "lister" && model == "gpt-6-luna"),
        "{lane:?}"
    );
    assert!(
        lane.iter()
            .any(|(kind, ..)| matches!(kind, BlockKind::ToolCall))
    );
    assert!(
        lane.iter()
            .any(|(kind, ..)| matches!(kind, BlockKind::Text))
    );
    assert_eq!(
        spawn_result(&events, calls[0].block),
        Some(("listed".to_owned(), None, "orchestrator".to_owned()))
    );
    assert_eq!(diagnostics(&events), []);
    match translator.finish_turn() {
        AdapterEvent::RunFinished { usage, .. } => {
            let child = usage
                .iter()
                .find(|entry| entry.agent == "lister")
                .expect("child usage");
            assert_eq!(child.model, "gpt-6-luna");
            assert_eq!(child.output_tokens, 38);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn subagent_from_spawn_tool_call_has_its_model_prompt_and_lane() {
    let mut translator = Translator::new(V1_MAIN, "gpt-5.6-luna");
    translator.begin_turn(V1_TURN, command_id());
    let out = feed_frames(&mut translator, &codex::replay("subagent_live_v1"));
    let events = events_of(&out);
    let calls = spawn_calls(&events);
    assert_eq!(calls.len(), 1, "{out:?}");
    assert_eq!(
        calls[0].summary.as_deref(),
        Some("Run the shell command `ls`, then reply with exactly the word `listed`.")
    );
    assert_ne!(
        calls[0].model, "",
        "model is empty only in the started frame"
    );
    let lane = child_blocks(&events, calls[0].block);
    assert!(
        lane.iter()
            .all(|(_, agent, model)| agent == "subagent" && model == "gpt-5.6-luna"),
        "{lane:?}"
    );
    assert!(
        lane.iter()
            .any(|(kind, ..)| matches!(kind, BlockKind::ToolCall))
    );
    assert_eq!(
        spawn_result(&events, calls[0].block).map(|(text, ..)| text),
        Some("listed".to_owned())
    );
    assert_eq!(diagnostics(&events), []);
}

#[test]
fn child_frames_ahead_of_the_receiver_id_keep_their_order_and_lane() {
    let mut translator = Translator::new("thread-main", "gpt-5.6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = translator.feed("item/started", &spawn_item("inProgress", &[]));
    let call = spawn_calls(&events_of(&out))[0].block;
    let early = translator.feed(
        "item/started",
        &json!({"threadId": "thread-child-1", "turnId": "turn-child", "item": {
            "id": "msg-c1", "type": "agentMessage", "text": ""}}),
    );
    assert!(events_of(&early).is_empty(), "{early:?}");
    let out = translator.feed(
        "item/completed",
        &spawn_item("completed", &["thread-child-1"]),
    );
    let events = events_of(&out);
    assert_eq!(child_blocks(&events, call).len(), 1, "{out:?}");
    assert_eq!(diagnostics(&events), []);
}

#[test]
fn subagent_outliving_the_turn_stays_attributed_and_leaves_the_phase_alone() {
    let mut translator = Translator::new(LATE_MAIN, "gpt-6-luna");
    translator.begin_turn(LATE_TURN, command_id());
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut ended = false;
    for frame in codex::replay("subagent_live_outlives_turn") {
        let Frame::Out(data) = frame else { continue };
        let method = data["method"].as_str().unwrap().to_owned();
        let out = translator.feed(&method, &data["params"]);
        let finished = out
            .iter()
            .any(|item| matches!(item, Translated::TurnCompleted { .. }));
        if ended {
            after.extend(out);
        } else {
            before.extend(out);
        }
        if finished {
            translator.finish_turn();
            ended = true;
        }
    }
    let calls = spawn_calls(&events_of(&before));
    assert_eq!(calls.len(), 1);
    let late = events_of(&after);
    assert_eq!(
        spawn_result(&late, calls[0].block).map(|(text, ..)| text),
        Some("woke".to_owned()),
        "{after:?}"
    );
    assert!(
        !late.iter().any(|e| matches!(e, AdapterEvent::Phase { .. })),
        "the idle session must not look busy: {after:?}"
    );
    assert_eq!(
        diagnostics(&late),
        [(
            "child_usage_outside_turn".to_owned(),
            "child_usage_outside_turn".to_owned()
        )]
    );
}

fn activity(thread: &str, turn: &str, child: &str, kind: &str) -> Value {
    json!({"threadId": thread, "turnId": turn, "item": {
        "id": format!("act-{child}-{kind}"), "type": "subAgentActivity", "kind": kind,
        "agentThreadId": child, "agentPath": "/root/a", "model": "gpt-6-luna"}})
}

fn child_turn_end(child: &str, status: &str, error: Value) -> Value {
    json!({"threadId": child, "turn": {"id": "turn-child", "status": status, "error": error,
        "items": [{"type": "agentMessage", "id": "m", "text": "partial"}]}})
}

#[test]
fn child_turn_status_reaches_the_result_block() {
    for (status, error, summary) in [
        ("failed", json!({"message": "boom"}), Some("error")),
        ("interrupted", Value::Null, Some("interrupted")),
        ("completed", Value::Null, None),
    ] {
        let mut translator = Translator::new("thread-main", "gpt-6-luna");
        translator.begin_turn("turn-main", command_id());
        let out = translator.feed(
            "item/started",
            &activity("thread-main", "turn-main", "child", "started"),
        );
        let call = spawn_calls(&events_of(&out))[0].block;
        let out = translator.feed("turn/completed", &child_turn_end("child", status, error));
        let (text, got, _) = spawn_result(&events_of(&out), call).expect("result");
        assert_eq!(text, "partial");
        assert_eq!(got.as_deref(), summary, "{status}");
    }
}

#[test]
fn failed_spawn_tool_call_reports_an_error_result() {
    let mut translator = Translator::new("thread-main", "gpt-5.6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = translator.feed("item/started", &spawn_item("inProgress", &[]));
    let call = spawn_calls(&events_of(&out))[0].block;
    let out = translator.feed("item/completed", &spawn_item("failed", &[]));
    let (_, summary, _) = spawn_result(&events_of(&out), call).expect("result");
    assert_eq!(summary.as_deref(), Some("error"));
}

#[test]
fn nested_subagent_hangs_under_the_call_of_its_parent() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = translator.feed(
        "item/started",
        &activity("thread-main", "turn-main", "child", "started"),
    );
    let outer = spawn_calls(&events_of(&out))[0].block;
    let out = translator.feed(
        "item/started",
        &activity("child", "turn-child", "grandchild", "started"),
    );
    let events = events_of(&out);
    let inner = spawn_calls(&events);
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].agent, "a");
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted { block_id, parent: Some(parent), .. }
            if *block_id == inner[0].block && *parent == outer
    )));
}

#[test]
fn activity_of_an_unknown_child_adopts_it() {
    // Потомок прошлого процесса вернулся через followup_task.
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = translator.feed(
        "item/started",
        &activity("thread-main", "turn-main", "child", "interacted"),
    );
    assert_eq!(spawn_calls(&events_of(&out)).len(), 1, "{out:?}");
}

#[test]
fn restart_closes_the_calls_of_running_children() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let out = translator.feed(
        "item/started",
        &activity("thread-main", "turn-main", "child", "started"),
    );
    let call = spawn_calls(&events_of(&out))[0].block;
    let closed = translator.abandon_children();
    let closed: Vec<&AdapterEvent> = closed.iter().collect();
    assert_eq!(
        spawn_result(&closed, call)
            .and_then(|(_, summary, _)| summary)
            .as_deref(),
        Some("interrupted")
    );
    assert!(translator.abandon_children().is_empty());
}

#[test]
fn unattributed_thread_frames_end_in_one_diagnostic_per_method() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let delta = json!({"threadId": "stranger", "turnId": "t", "itemId": "i", "delta": "secret"});
    for _ in 0..3 {
        let held = translator.feed("item/agentMessage/delta", &delta);
        assert!(events_of(&held).is_empty(), "{held:?}");
    }
    let out = translator.feed(
        "turn/completed",
        &json!({"threadId": "thread-main",
            "turn": {"id": "turn-main", "status": "completed", "items": []}}),
    );
    let events = events_of(&out);
    let found: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::Diagnostic {
                kind,
                event_type,
                field_names,
                ..
            } => Some((*kind, event_type.clone(), field_names.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(found.len(), 1, "{out:?}");
    assert_eq!(found[0].0, "unattributed_thread_frame");
    assert_eq!(found[0].1, "item/agentMessage/delta");
    assert!(found[0].2.contains(&"delta".to_owned()));
    assert!(!format!("{found:?}").contains("secret"));
}

#[test]
fn unattributed_frames_outside_a_turn_are_reported_at_once() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    let out = translator.feed(
        "item/started",
        &json!({"threadId": "stranger", "turnId": "t", "item": {
            "id": "m", "type": "agentMessage", "text": ""}}),
    );
    assert_eq!(
        diagnostics(&events_of(&out)),
        [(
            "unattributed_thread_frame".to_owned(),
            "item/started".to_owned()
        )]
    );
}

#[test]
fn unattributed_buffer_is_bounded() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    translator.begin_turn("turn-main", command_id());
    let delta = json!({"threadId": "stranger", "turnId": "t", "itemId": "i", "delta": "x"});
    let mut reported = 0;
    for _ in 0..2_000 {
        reported += diagnostics(&events_of(
            &translator.feed("item/agentMessage/delta", &delta),
        ))
        .len();
    }
    assert_eq!(reported, 1, "overflow is reported, not silently dropped");
}

#[test]
fn another_thread_without_a_parent_raises_a_diagnostic() {
    let mut translator = Translator::new("thread-main", "gpt-6-luna");
    let out = translator.feed(
        "thread/started",
        &json!({"thread": {"id": "other", "parentThreadId": null}}),
    );
    assert_eq!(
        diagnostics(&events_of(&out)),
        [(
            "unattributed_thread_frame".to_owned(),
            "thread/started".to_owned()
        )]
    );
}

#[test]
fn goal_cleared_after_resume_is_known_and_silent() {
    let mut translator = Translator::new("01a196b0-d5f1-7af1-8e3f-d8749c706b15", "gpt-6-luna");
    let out = feed_frames(&mut translator, &codex::replay("goal_cleared_live"));
    assert!(events_of(&out).is_empty(), "{out:?}");
}
