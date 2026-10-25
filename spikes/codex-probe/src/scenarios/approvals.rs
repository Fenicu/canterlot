//! Разрешения (command/fileChange approval) и структурированные вопросы.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{TURN_WAIT, Verdict, has_word};
use crate::appserver::Event;
use crate::driver::{
    Action, RPC_WAIT, Session, SessionOptions, TurnState, drive_turn, normalize_command, text_input,
};
use crate::evidence::Recorder;
use crate::isolation;
use crate::protocol::{
    self, CommandApprovalParams, FileChangeApprovalParams, RequestId, ThreadItem, TurnStatus,
    UserInputParams,
};

pub async fn permissions(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    let probe = scratch.join("probe.txt");
    let note = scratch.join("note.txt");

    let mut log: Vec<Value> = Vec::new();
    let mut accepted: Option<(RequestId, String)> = None;
    let mut declined_file: Option<(RequestId, String)> = None;
    let out = {
        let mut hook = |ev: &Event, st: &TurnState| -> Vec<Action> {
            let at = st.started.elapsed().as_millis() as u64;
            match ev {
                Event::ServerRequest { id, method, params }
                    if method == "item/commandExecution/requestApproval" =>
                {
                    let Ok(p) = serde_json::from_value::<CommandApprovalParams>(params.clone())
                    else {
                        return Vec::new();
                    };
                    let cmd = p.command.clone().unwrap_or_default();
                    let started_item = st
                        .items_started
                        .iter()
                        .any(|i| i.id() == Some(p.item_id.as_str()));
                    let exists_before = probe.exists();
                    let is_target =
                        normalize_command(&cmd) == "touch probe.txt" && accepted.is_none();
                    let decision = if is_target { "accept" } else { "decline" };
                    log.push(json!({ "at_ms": at, "request": "commandExecution", "requestId": id.to_value(),
                        "threadId": p.thread_id, "turnId": p.turn_id, "itemId": p.item_id,
                        "approvalId": p.approval_id, "command": cmd, "reason": p.reason,
                        "availableDecisions": p.available_decisions,
                        "item_started_before_request": started_item,
                        "probe_exists_before_decision": exists_before, "decision": decision }));
                    if is_target {
                        accepted = Some((id.clone(), p.item_id.clone()));
                    }
                    vec![Action::Respond {
                        id: id.clone(),
                        result: protocol::approval_decision(decision),
                    }]
                }
                Event::ServerRequest { id, method, params }
                    if method == "item/fileChange/requestApproval" =>
                {
                    let Ok(p) = serde_json::from_value::<FileChangeApprovalParams>(params.clone())
                    else {
                        return Vec::new();
                    };
                    log.push(
                        json!({ "at_ms": at, "request": "fileChange", "requestId": id.to_value(),
                        "threadId": p.thread_id, "turnId": p.turn_id, "itemId": p.item_id,
                        "reason": p.reason, "grantRoot": p.grant_root,
                        "note_exists_before_decision": note.exists(), "decision": "decline" }),
                    );
                    declined_file = Some((id.clone(), p.item_id.clone()));
                    vec![Action::Respond {
                        id: id.clone(),
                        result: protocol::approval_decision("decline"),
                    }]
                }
                Event::Notification { method, params } if method == "serverRequest/resolved" => {
                    log.push(json!({ "at_ms": at, "resolved": params["requestId"] }));
                    Vec::new()
                }
                Event::Notification { method, params } if method == "item/completed" => {
                    let ty = params["item"]["type"].as_str().unwrap_or_default();
                    if ty == "commandExecution" || ty == "fileChange" {
                        log.push(json!({ "at_ms": at, "item_completed": ty, "itemId": params["item"]["id"],
                            "status": params["item"]["status"], "exitCode": params["item"]["exitCode"],
                            "probe_exists_now": probe.exists(), "note_exists_now": note.exists() }));
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            }
        };
        drive_turn(
            &mut s.srv,
            &t.thread.id,
            json!({ "input": text_input(
                "Do exactly two things in this order. 1) Run the shell command `touch probe.txt` in the current directory; the sandbox is read-only, so request escalated permissions for that command. 2) Create a new file named note.txt containing the single line hi using your apply_patch file editing tool, not the shell. If any action is declined, do not retry it in any other way. Finally reply with the single word DONE."
            ) }),
            &mut hook,
            Duration::from_secs(240),
        )
        .await?
    };
    rec.note(json!({ "approval_log": log }));

    rec.step("send a duplicate decision for the already-resolved command approval");
    let mut duplicate_effect = Value::Null;
    if let Some((id, _)) = &accepted {
        let before: Vec<String> = commands(&out.items_completed);
        s.srv
            .respond(id, protocol::approval_decision("accept"))
            .await?;
        let until = Instant::now() + Duration::from_secs(3);
        let mut seen = Vec::new();
        while let Some(rem) = until.checked_duration_since(Instant::now()) {
            match s.srv.next_event(rem).await {
                Some(Event::Notification { method, .. }) => seen.push(method),
                Some(Event::ServerRequest { method, .. }) => seen.push(format!("request:{method}")),
                _ => break,
            }
        }
        duplicate_effect = json!({ "events_after_duplicate": seen, "commands_before": before });
    }
    rec.note(json!({ "duplicate_decision": duplicate_effect }));
    s.srv.shutdown().await?;

    let probe_after = probe.exists();
    let note_after = note.exists();
    let accepted_ok = log
        .iter()
        .any(|l| l["decision"] == "accept" && l["probe_exists_before_decision"] == false)
        && probe_after;
    let declined_surface = out.items_completed.iter().any(|i| {
        matches!(i,
        ThreadItem::FileChange { status, .. } if status == "declined")
    });
    rec.note(
        json!({ "final_files": { "probe.txt": probe_after, "note.txt": note_after },
        "fileChange_declined_item": declined_surface, "turn_status": format!("{:?}", out.status) }),
    );
    Ok(
        if accepted_ok && declined_file.is_some() && declined_surface && !note_after {
            Verdict::new(
                "PASS",
                "command approval accepted -> probe.txt created only after accept; fileChange approval declined -> item completed with status declined and note.txt absent; requests carry threadId/turnId/itemId",
            )
        } else if accepted_ok {
            Verdict::new(
                "PARTIAL",
                format!(
                    "command approval proven; fileChange approval not observed as expected (declined_request={}, declined_item={declined_surface}, note.txt={note_after})",
                    declined_file.is_some()
                ),
            )
        } else {
            Verdict::new(
                "FAIL",
                "approval flow not observed as scripted; see approval_log",
            )
        },
    )
}

fn commands(items: &[ThreadItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            ThreadItem::CommandExecution { command, .. } => Some(command.clone()),
            _ => None,
        })
        .collect()
}

const QUESTION_PROMPT: &str = "Before doing anything else, ask me exactly one question using the request_user_input tool: header 'Color', question 'Which color do you prefer?', options 'Red' and 'Blue'. After I answer, do not write a plan; reply with only the chosen color in uppercase.";

fn answer_hook<'a>(log: &'a mut Vec<Value>) -> impl FnMut(&Event, &TurnState) -> Vec<Action> + 'a {
    move |ev: &Event, st: &TurnState| {
        let at = st.started.elapsed().as_millis() as u64;
        match ev {
            Event::ServerRequest { id, method, params }
                if method == "item/tool/requestUserInput" =>
            {
                let parsed = serde_json::from_value::<UserInputParams>(params.clone());
                let Ok(p) = parsed else {
                    log.push(json!({ "at_ms": at, "unparsed_request_user_input": params }));
                    return Vec::new();
                };
                let mut answers: Vec<(String, Vec<String>)> = Vec::new();
                for q in &p.questions {
                    let labels: Vec<String> = q
                        .options
                        .as_ref()
                        .map(|o| o.iter().map(|x| x.label.clone()).collect())
                        .unwrap_or_default();
                    // выбираем только вариант, который допускает агент
                    let pick = labels
                        .iter()
                        .find(|l| l.eq_ignore_ascii_case("blue"))
                        .or_else(|| labels.last())
                        .cloned();
                    if let Some(pick) = pick {
                        answers.push((q.id.clone(), vec![pick]));
                    }
                }
                log.push(
                    json!({ "at_ms": at, "requestId": id.to_value(), "threadId": p.thread_id,
                    "turnId": p.turn_id, "itemId": p.item_id, "isBlocking": p.is_blocking,
                    "questions": params["questions"], "answers": answers }),
                );
                let refs: Vec<(&str, Vec<String>)> = answers
                    .iter()
                    .map(|(q, a)| (q.as_str(), a.clone()))
                    .collect();
                vec![Action::Respond {
                    id: id.clone(),
                    result: protocol::user_input_answer(&refs),
                }]
            }
            Event::Notification { method, params } if method == "serverRequest/resolved" => {
                log.push(json!({ "at_ms": at, "resolved": params["requestId"] }));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

pub async fn question(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    rec.step("part A: stable API surface (experimentalApi=false)");
    let mut a = Session::open(rec, scratch, SessionOptions::default()).await?;
    let ta = a.start_thread(false).await?;
    let plan_mode = json!({ "mode": "plan", "settings": {
        "model": isolation::MODEL, "reasoning_effort": isolation::EFFORT, "developer_instructions": null } });
    let rejected = a
        .srv
        .call(
            "turn/start",
            json!({ "threadId": ta.thread.id, "input": text_input("Reply OK."), "collaborationMode": plan_mode }),
            RPC_WAIT,
        )
        .await?;
    let plan_rejected = rejected.is_err();
    rec.note(
        json!({ "collaborationMode_without_experimentalApi": match &rejected {
        Ok(v) => json!({ "accepted": v }), Err(e) => json!({ "rejected": e }) } }),
    );
    if let Ok(v) = &rejected {
        // если вдруг принят: дождаться завершения, чтобы не оставлять ход висеть
        let turn_id = v["turn"]["id"].as_str().unwrap_or_default().to_string();
        wait_turn_completed(&mut a, &turn_id, TURN_WAIT).await;
    }
    let mut log_a: Vec<Value> = Vec::new();
    let out_a = {
        let mut hook = answer_hook(&mut log_a);
        drive_turn(
            &mut a.srv,
            &ta.thread.id,
            json!({ "input": text_input(QUESTION_PROMPT) }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let stable_asked = out_a
        .server_requests
        .iter()
        .any(|(_, m, _)| m == "item/tool/requestUserInput");
    rec.note(
        json!({ "stable_surface": { "request_user_input_received": stable_asked,
        "log": log_a, "agent_text": out_a.agent_text } }),
    );
    a.srv.shutdown().await?;

    rec.step("part B: experimentalApi=true + collaborationMode plan");
    let mut b = Session::open(
        rec,
        scratch,
        SessionOptions {
            experimental: true,
            ..SessionOptions::default()
        },
    )
    .await?;
    let modes = b
        .srv
        .call("collaborationMode/list", json!({}), RPC_WAIT)
        .await?;
    rec.note(json!({ "collaborationMode_list": match &modes { Ok(v) => v.clone(), Err(e) => json!({"error": e}) } }));
    let tb = b.start_thread(false).await?;
    let mut log_b: Vec<Value> = Vec::new();
    let out_b = {
        let mut hook = answer_hook(&mut log_b);
        drive_turn(
            &mut b.srv,
            &tb.thread.id,
            json!({ "input": text_input(QUESTION_PROMPT), "collaborationMode": plan_mode }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let plan_text: Vec<String> = out_b
        .items_completed
        .iter()
        .filter_map(|i| match i {
            ThreadItem::Plan { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let asked = log_b.iter().any(|l| l.get("requestId").is_some());
    let answered_blue = log_b
        .iter()
        .any(|l| l["answers"].to_string().to_lowercase().contains("blue"));
    let model_used_answer =
        has_word(&out_b.agent_text, "BLUE") || plan_text.iter().any(|p| has_word(p, "blue"));
    rec.note(json!({ "experimental_surface": { "request_user_input_received": asked,
        "answered": answered_blue, "agent_text": out_b.agent_text, "plan_items": plan_text,
        "model_reflected_answer": model_used_answer, "turn_status": format!("{:?}", out_b.status) } }));
    b.srv.shutdown().await?;

    Ok(
        if asked
            && answered_blue
            && model_used_answer
            && out_b.status == Some(TurnStatus::Completed)
        {
            Verdict::new(
                "PARTIAL",
                format!(
                    "structured question works only through the EXPERIMENTAL surface (experimentalApi + collaborationMode plan; ToolRequestUserInput* types are marked EXPERIMENTAL); answer 'Blue' reached the model. Stable surface: collaborationMode rejected={plan_rejected}, request_user_input received={stable_asked}"
                ),
            )
        } else if stable_asked {
            Verdict::new(
                "PASS",
                "request_user_input observed on the stable surface; see notes",
            )
        } else {
            Verdict::new(
                "BLOCKED",
                format!(
                    "no structured question round-trip proven (experimental asked={asked}, answered={answered_blue}, reflected={model_used_answer})"
                ),
            )
        },
    )
}

async fn wait_turn_completed(s: &mut Session, turn_id: &str, wait: Duration) {
    let until = Instant::now() + wait;
    while let Some(rem) = until.checked_duration_since(Instant::now()) {
        match s.srv.next_event(rem).await {
            Some(Event::Notification { method, params })
                if method == "turn/completed" && params["turn"]["id"] == turn_id =>
            {
                return;
            }
            Some(Event::StdoutClosed) | None => return,
            _ => {}
        }
    }
}

/// Вопрос в режиме Default на стабильном соединении с фичей в статусе under development.
pub async fn question_default_mode(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    rec.step(
        "stable connection (experimentalApi=false) + --enable default_mode_request_user_input",
    );
    let mut s = Session::open(
        rec,
        scratch,
        SessionOptions {
            extra_args: vec!["--enable".into(), "default_mode_request_user_input".into()],
            ..SessionOptions::default()
        },
    )
    .await?;
    let t = s.start_thread(false).await?;
    let mut log: Vec<Value> = Vec::new();
    let out = {
        let mut hook = answer_hook(&mut log);
        drive_turn(
            &mut s.srv,
            &t.thread.id,
            json!({ "input": text_input(QUESTION_PROMPT) }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let asked = log.iter().any(|l| l.get("requestId").is_some());
    let reflected = has_word(&out.agent_text, "BLUE");
    rec.note(
        json!({ "default_mode_flag": { "request_user_input_received": asked,
        "log": log, "agent_text": out.agent_text, "turn_status": format!("{:?}", out.status) } }),
    );
    s.srv.shutdown().await?;
    Ok(if asked && reflected {
        Verdict::new(
            "PARTIAL",
            "request_user_input works in Default mode on a stable connection only with the under-development feature flag default_mode_request_user_input",
        )
    } else {
        Verdict::new(
            "BLOCKED",
            format!(
                "Default mode with the feature flag did not produce a structured question round-trip (asked={asked}, reflected={reflected})"
            ),
        )
    })
}
