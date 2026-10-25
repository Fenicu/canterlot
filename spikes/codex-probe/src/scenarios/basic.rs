//! Поток текста, несколько вводов, подтверждение приёма, инструменты.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{TURN_WAIT, Verdict, has_word};
use crate::appserver::Event;
use crate::driver::{
    Action, RPC_WAIT, Session, SessionOptions, TurnState, drive_turn, simple_turn, text_input,
};
use crate::evidence::Recorder;
use crate::protocol::{ThreadItem, TurnStatus};

pub async fn start_stream(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    rec.note(
        json!({ "native_thread_id": t.thread.id, "sessionId": t.thread.session_id,
        "rollout_path": t.thread.path }),
    );

    let mut usage: Vec<Value> = Vec::new();
    let mut statuses: Vec<Value> = Vec::new();
    let mut lifecycle: Vec<Value> = Vec::new();
    let out = {
        let mut hook = |ev: &Event, _: &TurnState| {
            if let Event::Notification { method, params } = ev {
                match method.as_str() {
                    "thread/tokenUsage/updated" => usage.push(params["tokenUsage"].clone()),
                    "thread/status/changed" => statuses.push(params["status"].clone()),
                    "item/started" | "item/completed" => lifecycle.push(json!({
                        "event": method, "type": params["item"]["type"], "id": params["item"]["id"]
                    })),
                    _ => {}
                }
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &t.thread.id,
            json!({ "input": text_input("Reply with the single word PONG and nothing else."),
                "effort": crate::isolation::EFFORT }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    rec.note(
        json!({ "item_lifecycle": lifecycle, "thread_status_changes": statuses,
        "token_usage_last": usage.last() }),
    );
    s.srv.shutdown().await?;

    let has_user = out
        .items_completed
        .iter()
        .any(|i| matches!(i, ThreadItem::UserMessage { .. }));
    let ok = out.status == Some(TurnStatus::Completed)
        && out.delta_count > 0
        && has_word(&out.agent_text, "PONG")
        && has_user;
    Ok(if ok {
        Verdict::new(
            "PASS",
            format!(
                "thread {} turn {} completed; {} agentMessage deltas; final text '{}'; token usage reported: {}",
                t.thread.id,
                out.turn_id,
                out.delta_count,
                out.agent_text.trim(),
                !usage.is_empty()
            ),
        )
    } else {
        Verdict::new(
            "FAIL",
            format!(
                "unexpected outcome: status {:?}, text '{}'",
                out.status, out.agent_text
            ),
        )
    })
}

pub async fn multi_input(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    let tid = t.thread.id.clone();

    rec.step("three sequential turns in one app-server process");
    let o1 = simple_turn(
        &mut s.srv,
        &tid,
        "Remember the word ALPHA for later. Reply with the single word OK1.",
        Some("cu-multi-1"),
        TURN_WAIT,
    )
    .await?;
    let o2 = simple_turn(
        &mut s.srv,
        &tid,
        "Reply with the single word OK2.",
        Some("cu-multi-2"),
        TURN_WAIT,
    )
    .await?;
    let o3 = simple_turn(
        &mut s.srv,
        &tid,
        "What word did I ask you to remember? Reply with that word only.",
        Some("cu-multi-3"),
        TURN_WAIT,
    )
    .await?;
    let seq_ok = [&o1, &o2, &o3]
        .iter()
        .all(|o| o.status == Some(TurnStatus::Completed))
        && has_word(&o1.agent_text, "OK1")
        && has_word(&o2.agent_text, "OK2")
        && has_word(&o3.agent_text, "ALPHA");
    let distinct = o1.turn_id != o2.turn_id && o2.turn_id != o3.turn_id;
    rec.note(
        json!({ "sequential": { "ok": seq_ok, "distinct_turn_ids": distinct,
        "turns": [o1.turn_id, o2.turn_id, o3.turn_id] } }),
    );

    rec.step("turn 4: while a command runs, send a concurrent turn/start and a turn/steer");
    let mut fired = false;
    let o4 = {
        let mut hook = |ev: &Event, _: &TurnState| {
            if fired {
                return Vec::new();
            }
            if let Event::Notification { method, params } = ev
                && method == "item/started"
                && params["item"]["type"] == "commandExecution"
            {
                fired = true;
                return vec![
                    Action::StartTurn {
                        text: "Reply with the single word EXTRA.".into(),
                        client_id: "cu-concurrent-start".into(),
                    },
                    Action::Steer {
                        text: "Also include the word STEERED in your final reply.".into(),
                        client_id: "cu-steer".into(),
                    },
                ];
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &tid,
            json!({ "input": text_input("Run the shell command `sleep 6` and then reply with the single word DONE4."),
                "clientUserMessageId": "cu-multi-4" }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let user_msgs: Vec<Value> = o4
        .items_completed
        .iter()
        .filter_map(|i| match i {
            ThreadItem::UserMessage { id, client_id, .. } => {
                Some(json!({ "id": id, "clientId": client_id }))
            }
            _ => None,
        })
        .collect();
    rec.note(json!({ "concurrency": {
        "fired": fired,
        "action_results": o4.action_results.iter().map(|(l, r)| json!({ "action": l,
            "result": match r { Ok(v) => v.clone(), Err(e) => json!({"error": e}) } })).collect::<Vec<_>>(),
        "user_messages_in_turn4": user_msgs,
        "turn4_text": o4.agent_text,
        "other_turns": o4.other_turns.iter().map(|(id, st)| json!({"turnId": id, "status": format!("{st:?}")})).collect::<Vec<_>>(),
    }}));

    rec.step("thread/turns/list to see where the concurrent input landed");
    let turns = s
        .srv
        .call_ok(
            "thread/turns/list",
            json!({ "threadId": tid, "limit": 10, "sortDirection": "asc", "itemsView": "full" }),
            RPC_WAIT,
        )
        .await?;
    let summary: Vec<Value> = turns["data"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| {
            json!({
                "turnId": t["id"], "status": t["status"],
                "userMessages": t["items"].as_array().map(|items| items.iter()
                    .filter(|i| i["type"] == "userMessage")
                    .map(|i| json!({ "clientId": i["clientId"], "text": i["content"][0]["text"] }))
                    .collect::<Vec<_>>()),
                "agentText": t["items"].as_array().map(|items| items.iter()
                    .filter(|i| i["type"] == "agentMessage")
                    .map(|i| i["text"].clone()).collect::<Vec<_>>()),
            })
        })
        .collect();
    rec.note(json!({ "persisted_turns": summary }));
    s.srv.shutdown().await?;

    Ok(if seq_ok && distinct {
        Verdict::new(
            "PASS",
            "three sequential turns completed in one process with context carried (ALPHA recalled); concurrent turn/start and turn/steer behaviour recorded in notes",
        )
    } else {
        Verdict::new("FAIL", "sequential turns did not complete as expected")
    })
}

pub async fn delivery_ack(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    let tid = t.thread.id.clone();
    let client_id = "canterlot-ack-0001";

    rec.step("turn/start with clientUserMessageId; record order of acknowledgement facts");
    let mut facts: Vec<Value> = Vec::new();
    let out = {
        let mut hook = |ev: &Event, st: &TurnState| {
            let at = st.started.elapsed().as_millis() as u64;
            if let Event::Notification { method, params } = ev {
                match method.as_str() {
                    "turn/started" => facts.push(json!({ "fact": "turn/started", "at_ms": at,
                        "turnId": params["turn"]["id"] })),
                    "item/started" | "item/completed"
                        if params["item"]["type"] == "userMessage" =>
                    {
                        facts.push(
                            json!({ "fact": format!("{method} userMessage"), "at_ms": at,
                            "itemId": params["item"]["id"], "clientId": params["item"]["clientId"],
                            "turnId": params["turnId"] }),
                        )
                    }
                    "thread/status/changed" => facts.push(json!({ "fact": "thread/status/changed",
                        "at_ms": at, "status": params["status"] })),
                    _ => {}
                }
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &tid,
            json!({ "input": text_input("Reply with the single word ACK."), "clientUserMessageId": client_id }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    rec.note(json!({ "ack_facts": facts, "turn_start_response_ms": out.t_response_ms }));
    let live_echo = facts
        .iter()
        .any(|f| f["clientId"] == client_id && f["fact"] == "item/started userMessage");

    rec.step("thread/items/list: the accepted input is persisted with the same clientId");
    let items = s
        .srv
        .call_ok(
            "thread/items/list",
            json!({ "threadId": tid, "turnId": out.turn_id, "limit": 20 }),
            RPC_WAIT,
        )
        .await?;
    let persisted: Vec<Value> = items["data"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|e| e["item"]["type"] == "userMessage")
        .map(|e| json!({ "turnId": e["turnId"], "itemId": e["item"]["id"], "clientId": e["item"]["clientId"] }))
        .collect();
    let persisted_ok = persisted.iter().any(|p| p["clientId"] == client_id);
    rec.note(
        json!({ "persisted_user_messages": persisted, "persisted_with_client_id": persisted_ok }),
    );

    rec.step("repeat turn/start with the SAME clientUserMessageId: does the server deduplicate?");
    let dup = simple_turn(
        &mut s.srv,
        &tid,
        "Reply with the single word ACK2.",
        Some(client_id),
        TURN_WAIT,
    )
    .await?;
    let deduplicated = dup.start_error.is_some() || dup.turn_id == out.turn_id;
    rec.note(json!({ "duplicate_client_id": {
        "rejected": dup.start_error.as_ref().map(|e| json!(e)),
        "new_turn_id": dup.turn_id, "first_turn_id": out.turn_id,
        "second_turn_ran": dup.status == Some(TurnStatus::Completed),
        "server_deduplicated": deduplicated,
    }}));
    s.srv.shutdown().await?;

    Ok(
        if live_echo && persisted_ok && out.t_response_ms.is_some() {
            Verdict::new(
                "PASS",
                format!(
                    "turn/start response carried turn id after {:?} ms; userMessage item echoed clientId live and in thread/items/list; duplicate clientId deduplicated by server: {deduplicated}",
                    out.t_response_ms
                ),
            )
        } else {
            Verdict::new("PARTIAL", "acknowledgement facts incomplete; see notes")
        },
    )
}

pub async fn tools(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    let mut delta_items: Vec<String> = Vec::new();
    let out = {
        let mut hook = |ev: &Event, _: &TurnState| {
            if let Event::Notification { method, params } = ev
                && method == "item/commandExecution/outputDelta"
                && let Some(id) = params["itemId"].as_str()
            {
                delta_items.push(id.to_string());
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &t.thread.id,
            json!({ "input": text_input(
                "Run exactly these two shell commands as two separate tool calls, one after another: first `echo canterlot-tool-ok`, then `ls /nonexistent-canterlot-dir`. Do not run anything else. Then reply with the two exit codes separated by a space."
            ) }),
            &mut hook,
            Duration::from_secs(240),
        )
        .await?
    };
    let cmds: Vec<Value> = out
        .items_completed
        .iter()
        .filter_map(|i| match i {
            ThreadItem::CommandExecution {
                id,
                command,
                status,
                exit_code,
                aggregated_output,
                ..
            } => Some(json!({
                "itemId": id, "command": command, "status": status, "exitCode": exit_code,
                "output": aggregated_output, "has_output_deltas": delta_items.contains(id),
            })),
            _ => None,
        })
        .collect();
    rec.note(json!({ "command_items": cmds, "output_delta_item_ids": delta_items }));
    s.srv.shutdown().await?;
    let ok_cmd = cmds.iter().any(|c| {
        c["exitCode"] == 0
            && c["command"]
                .as_str()
                .is_some_and(|x| x.contains("echo canterlot-tool-ok"))
    });
    let fail_cmd = cmds.iter().any(|c| {
        c["exitCode"].as_i64().is_some_and(|e| e != 0)
            && c["command"]
                .as_str()
                .is_some_and(|x| x.contains("nonexistent-canterlot-dir"))
    });
    Ok(
        if ok_cmd && fail_cmd && out.status == Some(TurnStatus::Completed) {
            Verdict::new(
                "PASS",
                "successful and failing commandExecution items with exit codes, output and item-id linkage; turn completed after a failing tool",
            )
        } else {
            Verdict::new(
                "PARTIAL",
                "expected command items not both observed; see notes",
            )
        },
    )
}

pub async fn image_input(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let img = scratch.join("red.png");
    std::fs::write(&img, crate::png::solid_rgb(64, [220, 20, 20]))?;
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    let mut hook = |_: &Event, _: &TurnState| Vec::new();
    let out = drive_turn(
        &mut s.srv,
        &t.thread.id,
        json!({ "input": [
            { "type": "text", "text": "What is the dominant color of the attached image? Reply with one word.", "text_elements": [] },
            { "type": "localImage", "path": img.to_string_lossy() }
        ] }),
        &mut hook,
        TURN_WAIT,
    )
    .await?;
    let echoed: Vec<Value> = out
        .items_completed
        .iter()
        .filter_map(|i| match i {
            ThreadItem::UserMessage { content, .. } => Some(json!(
                content
                    .iter()
                    .map(|c| c["type"].clone())
                    .collect::<Vec<_>>()
            )),
            _ => None,
        })
        .collect();
    rec.note(json!({ "user_message_content_types": echoed, "answer": out.agent_text }));
    s.srv.shutdown().await?;
    Ok(
        if out.status == Some(TurnStatus::Completed) && has_word(&out.agent_text, "red") {
            Verdict::new(
                "PASS",
                format!(
                    "localImage input accepted; model answered '{}'",
                    out.agent_text.trim()
                ),
            )
        } else {
            Verdict::new(
                "FAIL",
                format!(
                    "image not understood: status {:?}, answer '{}'",
                    out.status, out.agent_text
                ),
            )
        },
    )
}
