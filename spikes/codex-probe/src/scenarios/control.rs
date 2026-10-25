//! Прерывание, возобновление, жизненный цикл процесса и фоновые терминалы.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{TURN_WAIT, Verdict, has_word, resume_params};
use crate::appserver::Event;
use crate::driver::{
    Action, RPC_WAIT, Session, SessionOptions, TurnState, drive_turn, simple_turn, text_input,
};
use crate::evidence::Recorder;
use crate::proctree::{self, ProcInfo};
use crate::protocol::{self, RequestId, ThreadItem, TurnStatus};

fn tree(root: u32) -> Vec<ProcInfo> {
    proctree::descendants(&proctree::snapshot(), root as i32)
}

fn tree_json(t: &[ProcInfo]) -> Value {
    json!(t)
}

/// Добивает только процессы из нашего снимка, если это всё ещё те же процессы (pid + время старта).
fn reap_survivors(rec: &Recorder, procs: &[ProcInfo]) -> Vec<ProcInfo> {
    let survivors: Vec<ProcInfo> = procs
        .iter()
        .filter(|p| proctree::still_alive(p))
        .cloned()
        .collect();
    for p in &survivors {
        let _ = crate::appserver::signal_pid(p.pid, libc::SIGKILL);
    }
    if !survivors.is_empty() {
        rec.note(json!({ "cleanup_killed_survivors": survivors }));
    }
    survivors
}

/// Проверяет живость процессов в контрольные моменты и собирает поздние события элементов.
async fn watch_processes(s: &mut Session, procs: &[ProcInfo], checkpoints_ms: &[u64]) -> Value {
    let t0 = Instant::now();
    let mut checks = Vec::new();
    let mut late_items = Vec::new();
    for &cp in checkpoints_ms {
        let target = Duration::from_millis(cp);
        while let Some(rem) = target.checked_sub(t0.elapsed()) {
            match s.srv.next_event(rem).await {
                Some(Event::Notification { method, params }) if method == "item/completed" => {
                    late_items.push(json!({ "at_ms": t0.elapsed().as_millis() as u64,
                        "type": params["item"]["type"], "itemId": params["item"]["id"],
                        "turnId": params["turnId"], "status": params["item"]["status"],
                        "exitCode": params["item"]["exitCode"] }));
                }
                Some(Event::StdoutClosed) | None => break,
                Some(_) => {}
            }
        }
        checks.push(json!({ "after_ms": cp,
            "alive": procs.iter().map(proctree::still_alive).collect::<Vec<_>>() }));
    }
    json!({ "checks": checks, "late_item_completions": late_items })
}

pub async fn interrupt(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = s.start_thread(false).await?;
    let tid = t.thread.id.clone();
    let root = s.srv.pid();

    rec.step("phase A: interrupt a running `sleep 30` command");
    let mut fired_at: Option<u128> = None;
    let mut sleep_procs: Vec<ProcInfo> = Vec::new();
    let out_a = {
        let mut hook = |ev: &Event, st: &TurnState| {
            if fired_at.is_some() {
                return Vec::new();
            }
            if let Event::Notification { method, params } = ev
                && method == "item/started"
                && params["item"]["type"] == "commandExecution"
            {
                // дать процессу стартовать, чтобы было что останавливать
                std::thread::sleep(Duration::from_millis(1500));
                sleep_procs = tree(root)
                    .into_iter()
                    .filter(|p| p.cmdline.contains("sleep 30"))
                    .collect();
                fired_at = Some(st.started.elapsed().as_millis());
                return vec![Action::Interrupt];
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &tid,
            json!({ "input": text_input("Run the shell command `sleep 30` and then reply with the single word DONE.") }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let ack_ms = out_a
        .action_results
        .iter()
        .find(|(l, _)| l == "turn/interrupt")
        .map(|(_, r)| r.is_ok());
    // окно наблюдения после terminal-состояния хода: убивает ли CLI команду сам
    let watch = watch_processes(&mut s, &sleep_procs, &[0, 2000, 5000, 10000]).await;
    let cmd_status: Vec<Value> = out_a
        .items_completed
        .iter()
        .filter_map(|i| match i {
            ThreadItem::CommandExecution { id, status, exit_code, duration_ms, .. } => {
                Some(json!({ "itemId": id, "status": status, "exitCode": exit_code, "durationMs": duration_ms }))
            }
            _ => None,
        })
        .collect();
    rec.note(json!({ "phase_a": {
        "interrupt_sent_at_ms": fired_at, "interrupt_response_ok": ack_ms,
        "turn_completed_at_ms": out_a.t_completed_ms, "final_status": format!("{:?}", out_a.status),
        "stop_latency_ms": match (fired_at, out_a.t_completed_ms) { (Some(a), Some(b)) => Some(b.saturating_sub(a) as u64), _ => None },
        "sleep_processes_before": sleep_procs, "post_interrupt_watch": watch,
        "command_items_completed_within_turn": cmd_status } }));
    reap_survivors(rec, &sleep_procs);

    rec.step("phase B: interrupt while an approval request is pending (no decision sent)");
    let probe2 = scratch.join("probe2.txt");
    let mut pending: Option<RequestId> = None;
    let mut log_b: Vec<Value> = Vec::new();
    let out_b = {
        let mut hook = |ev: &Event, st: &TurnState| -> Vec<Action> {
            let at = st.started.elapsed().as_millis() as u64;
            match ev {
                Event::ServerRequest { id, method, params }
                    if method == "item/commandExecution/requestApproval" && pending.is_none() =>
                {
                    log_b.push(json!({ "at_ms": at, "approval_request": id.to_value(),
                        "command": params["command"], "itemId": params["itemId"] }));
                    pending = Some(id.clone());
                    vec![Action::Defer { id: id.clone() }, Action::Interrupt]
                }
                Event::Notification { method, params } if method == "serverRequest/resolved" => {
                    log_b.push(json!({ "at_ms": at, "resolved": params["requestId"] }));
                    Vec::new()
                }
                _ => Vec::new(),
            }
        };
        drive_turn(
            &mut s.srv,
            &tid,
            json!({ "input": text_input("Run the shell command `touch probe2.txt` in the current directory; the sandbox is read-only, so request escalated permissions for it. Then reply with the single word DONE.") }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    rec.note(
        json!({ "phase_b": { "log": log_b, "final_status": format!("{:?}", out_b.status),
        "probe2_exists": probe2.exists() } }),
    );
    let mut late_effect = Value::Null;
    if let Some(id) = &pending {
        rec.step("late 'accept' for the cleared approval request");
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
        late_effect = json!({ "events": seen, "probe2_exists_after_late_accept": probe2.exists() });
    }
    rec.note(json!({ "late_decision": late_effect }));

    rec.step("phase C: thread still usable after interrupts");
    let out_c = simple_turn(
        &mut s.srv,
        &tid,
        "Reply with the single word AFTER.",
        None,
        TURN_WAIT,
    )
    .await?;
    s.srv.shutdown().await?;

    let a_ok = out_a.status == Some(TurnStatus::Interrupted) && ack_ms == Some(true);
    let c_ok = out_c.status == Some(TurnStatus::Completed) && has_word(&out_c.agent_text, "AFTER");
    Ok(if a_ok && c_ok {
        Verdict::new(
            "PASS",
            format!(
                "turn/interrupt acknowledged ({{}}), turn/completed status=interrupted after {:?} ms; pending approval case status={:?}, probe2 created={}; next turn completed",
                match (fired_at, out_a.t_completed_ms) {
                    (Some(a), Some(b)) => Some(b.saturating_sub(a)),
                    _ => None,
                },
                out_b.status,
                probe2.exists()
            ),
        )
    } else {
        Verdict::new(
            "FAIL",
            format!(
                "interrupt not proven: phase A status {:?}, phase C status {:?}",
                out_a.status, out_c.status
            ),
        )
    })
}

pub async fn resume(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    rec.step("phase 1: new thread, give a code word, stop the process cleanly (stdin EOF)");
    let mut a = Session::open(rec, scratch, SessionOptions::default()).await?;
    let t = a.start_thread(false).await?;
    let tid = t.thread.id.clone();
    let o1 = simple_turn(
        &mut a.srv,
        &tid,
        "Remember this code word: ZEBRA-42. Reply with the single word OK.",
        None,
        TURN_WAIT,
    )
    .await?;
    let t0 = Instant::now();
    a.srv.close_stdin().await;
    let exited = a.srv.wait_exit(Duration::from_secs(15)).await;
    rec.note(json!({ "phase1": { "turn_status": format!("{:?}", o1.status), "eof_exit_ms": t0.elapsed().as_millis() as u64,
        "exited": exited.is_some() } }));
    if exited.is_none() {
        a.srv.shutdown().await?;
    }

    rec.step("phase 2: new process, thread/resume by id, check context continuity");
    let mut b = Session::open(rec, scratch, SessionOptions::default()).await?;
    let r = b
        .srv
        .call_ok("thread/resume", resume_params(&tid, scratch), RPC_WAIT)
        .await?;
    rec.note(
        json!({ "resume_response": { "threadId": r["thread"]["id"], "status": r["thread"]["status"],
        "model": r["model"], "approvalPolicy": r["approvalPolicy"], "sandbox": r["sandbox"] } }),
    );
    let o2 = simple_turn(
        &mut b.srv,
        &tid,
        "What is the code word I asked you to remember? Reply with the code word only.",
        None,
        TURN_WAIT,
    )
    .await?;
    let recalled = has_word(&o2.agent_text, "ZEBRA-42");

    rec.step("phase 3: SIGKILL the app-server while a command runs");
    let root = b.srv.pid();
    let mut cmd_seen = false;
    let o3 = {
        let mut hook = |ev: &Event, _: &TurnState| {
            if let Event::Notification { method, params } = ev
                && method == "item/started"
                && params["item"]["type"] == "commandExecution"
            {
                cmd_seen = true;
                std::thread::sleep(Duration::from_millis(1500));
                return vec![Action::Stop];
            }
            Vec::new()
        };
        drive_turn(
            &mut b.srv,
            &tid,
            json!({ "input": text_input("Run the shell command `sleep 30` and then reply with the single word DONE."),
                "clientUserMessageId": "cu-killed-turn" }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let before = tree(root);
    rec.note(
        json!({ "process_tree_before_kill": tree_json(&before), "command_started": cmd_seen,
        "killed_turn_id": o3.turn_id }),
    );
    b.srv.signal(libc::SIGKILL)?;
    let _ = b.srv.wait_exit(Duration::from_secs(5)).await;
    std::thread::sleep(Duration::from_millis(1000));
    let survivors: Vec<ProcInfo> = before
        .iter()
        .filter(|p| p.pid != root as i32 && proctree::still_alive(p))
        .cloned()
        .collect();
    rec.note(json!({ "survivors_after_sigkill_of_direct_child": survivors }));
    reap_survivors(rec, &before);
    drop(b);

    rec.step("phase 4: new process, resume after crash, inspect the killed turn");
    let mut c = Session::open(rec, scratch, SessionOptions::default()).await?;
    let r2 = c
        .srv
        .call("thread/resume", resume_params(&tid, scratch), RPC_WAIT)
        .await?;
    rec.note(json!({ "resume_after_kill": match &r2 { Ok(v) => json!({ "status": v["thread"]["status"] }),
        Err(e) => json!({ "error": e }) } }));
    let turns = c
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
            json!({ "turnId": t["id"], "status": t["status"], "error": t["error"],
            "items": t["items"].as_array().map(|items| items.iter().map(|i| json!({
                "type": i["type"], "clientId": i.get("clientId"), "status": i.get("status"),
                "text": i.get("text") })).collect::<Vec<_>>()) })
        })
        .collect();
    let killed = summary
        .iter()
        .find(|t| t["turnId"] == o3.turn_id.as_str())
        .cloned();
    rec.note(json!({ "history_after_kill": summary, "killed_turn_state": killed }));
    let o4 = simple_turn(
        &mut c.srv,
        &tid,
        "Reply with the single word ALIVE.",
        None,
        TURN_WAIT,
    )
    .await?;
    c.srv.shutdown().await?;

    let ok = o1.status == Some(TurnStatus::Completed)
        && recalled
        && r2.is_ok()
        && o4.status == Some(TurnStatus::Completed);
    Ok(if ok {
        Verdict::new(
            "PASS",
            format!(
                "thread/resume in a new process recalled ZEBRA-42; after SIGKILL the thread resumed and accepted a new turn; killed turn persisted as {}",
                killed
                    .map(|k| k["status"].to_string())
                    .unwrap_or_else(|| "<absent>".into())
            ),
        )
    } else {
        Verdict::new(
            "FAIL",
            format!(
                "resume not proven: recalled={recalled}, resume_after_kill_ok={}",
                r2.is_ok()
            ),
        )
    })
}

pub async fn lifecycle(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut facts = Vec::new();
    for how in ["eof", "sigint", "sigterm"] {
        rec.step(&format!("lifecycle: {how}"));
        let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;
        let _t = s.start_thread(true).await?;
        let root = s.srv.pid();
        let before = tree(root);
        let t0 = Instant::now();
        match how {
            "eof" => s.srv.close_stdin().await,
            "sigint" => s.srv.signal(libc::SIGINT)?,
            _ => s.srv.signal(libc::SIGTERM)?,
        }
        let st = s.srv.wait_exit(Duration::from_secs(10)).await;
        let elapsed = t0.elapsed().as_millis() as u64;
        std::thread::sleep(Duration::from_millis(500));
        let survivors: Vec<ProcInfo> = before
            .iter()
            .filter(|p| proctree::still_alive(p))
            .cloned()
            .collect();
        facts.push(
            json!({ "how": how, "exited": st.is_some(), "exit": st.map(|s| format!("{s:?}")),
            "after_ms": elapsed, "tree_before": before, "survivors": survivors }),
        );
        if st.is_none() {
            s.srv.shutdown().await?;
        }
        reap_survivors(rec, &before);
    }
    rec.note(json!({ "lifecycle": facts }));
    let all_exit = facts.iter().all(|f| f["exited"] == true);
    let no_orphans = facts
        .iter()
        .all(|f| f["survivors"].as_array().is_some_and(|a| a.is_empty()));
    Ok(if all_exit && no_orphans {
        Verdict::new(
            "PASS",
            "app-server exits on stdin EOF, SIGINT and SIGTERM without leaving processes behind (idle, no running turn)",
        )
    } else {
        Verdict::new(
            "PARTIAL",
            format!("exit on all signals={all_exit}, no orphans={no_orphans}; see notes"),
        )
    })
}

pub async fn background(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(
        rec,
        scratch,
        SessionOptions {
            experimental: true,
            ..SessionOptions::default()
        },
    )
    .await?;
    let t = s.start_thread(false).await?;
    let tid = t.thread.id.clone();
    let root = s.srv.pid();
    let out = {
        let mut hook = |ev: &Event, _: &TurnState| -> Vec<Action> {
            // любые запросы одобрения здесь неожиданны: отклонить
            if let Event::ServerRequest { id, method, .. } = ev
                && method == "item/commandExecution/requestApproval"
            {
                return vec![Action::Respond {
                    id: id.clone(),
                    result: protocol::approval_decision("decline"),
                }];
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &tid,
            json!({ "input": text_input("Start the shell command `sleep 120` so that it keeps running in the background in a terminal session: do not wait for it to finish and do not stop it. Then immediately reply with the single word STARTED.") }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let listed = s
        .srv
        .call(
            "thread/backgroundTerminals/list",
            json!({ "threadId": tid }),
            RPC_WAIT,
        )
        .await?;
    rec.note(json!({ "background_terminals": match &listed { Ok(v) => v.clone(), Err(e) => json!({"error": e}) } }));
    let before = tree(root);
    let sleepers: Vec<ProcInfo> = before
        .iter()
        .filter(|p| p.cmdline.contains("sleep 120"))
        .cloned()
        .collect();
    rec.note(
        json!({ "process_tree_after_turn": before, "sleep_processes": sleepers,
        "turn_status": format!("{:?}", out.status), "agent_text": out.agent_text }),
    );

    rec.step("stop app-server with stdin EOF; do background processes outlive it?");
    let t0 = Instant::now();
    s.srv.close_stdin().await;
    let st = s.srv.wait_exit(Duration::from_secs(15)).await;
    std::thread::sleep(Duration::from_millis(1000));
    let survivors: Vec<ProcInfo> = before
        .iter()
        .filter(|p| p.pid != root as i32 && proctree::still_alive(p))
        .cloned()
        .collect();
    rec.note(
        json!({ "exit_after_eof_ms": t0.elapsed().as_millis() as u64, "exited": st.is_some(),
        "survivors": survivors }),
    );
    if st.is_none() {
        s.srv.shutdown().await?;
    }
    reap_survivors(rec, &before);
    let listed_any = listed
        .as_ref()
        .ok()
        .and_then(|v| v["data"].as_array().map(|a| !a.is_empty()))
        .unwrap_or(false);
    let survived_sleep = survivors.iter().any(|p| p.cmdline.contains("sleep 120"));
    Ok(if !sleepers.is_empty() {
        Verdict::new(
            "PASS",
            format!(
                "background `sleep 120` observed in process tree; listed by experimental thread/backgroundTerminals/list={listed_any}; survived app-server exit={survived_sleep}"
            ),
        )
    } else {
        Verdict::new(
            "PARTIAL",
            format!("model did not leave a background process (listed={listed_any}); see notes"),
        )
    })
}

/// После `turn/interrupt` команда продолжает жить: проверяем экспериментальную очистку фоновых терминалов.
pub async fn interrupt_cleanup(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    let mut s = Session::open(
        rec,
        scratch,
        SessionOptions {
            experimental: true,
            ..SessionOptions::default()
        },
    )
    .await?;
    let t = s.start_thread(false).await?;
    let tid = t.thread.id.clone();
    let root = s.srv.pid();
    let mut fired = false;
    let mut sleep_procs: Vec<ProcInfo> = Vec::new();
    let out = {
        let mut hook = |ev: &Event, _: &TurnState| {
            if !fired
                && let Event::Notification { method, params } = ev
                && method == "item/started"
                && params["item"]["type"] == "commandExecution"
            {
                std::thread::sleep(Duration::from_millis(1500));
                sleep_procs = tree(root)
                    .into_iter()
                    .filter(|p| p.cmdline.contains("sleep 45"))
                    .collect();
                fired = true;
                return vec![Action::Interrupt];
            }
            Vec::new()
        };
        drive_turn(
            &mut s.srv,
            &tid,
            json!({ "input": text_input("Run the shell command `sleep 45` and then reply with the single word DONE.") }),
            &mut hook,
            TURN_WAIT,
        )
        .await?
    };
    let alive_after_interrupt: Vec<bool> = sleep_procs.iter().map(proctree::still_alive).collect();
    let listed = s
        .srv
        .call(
            "thread/backgroundTerminals/list",
            json!({ "threadId": tid }),
            RPC_WAIT,
        )
        .await?;
    rec.note(json!({ "after_interrupt": { "turn_status": format!("{:?}", out.status),
        "sleep_processes": sleep_procs, "alive": alive_after_interrupt,
        "background_terminals": match &listed { Ok(v) => v.clone(), Err(e) => json!({"error": e}) } } }));
    let clean = s
        .srv
        .call(
            "thread/backgroundTerminals/clean",
            json!({ "threadId": tid }),
            RPC_WAIT,
        )
        .await?;
    let watch = watch_processes(&mut s, &sleep_procs, &[0, 1000, 3000]).await;
    rec.note(json!({ "clean_response": match &clean { Ok(v) => v.clone(), Err(e) => json!({"error": e}) },
        "after_clean": watch }));
    let stopped = watch["checks"]
        .as_array()
        .and_then(|c| c.last())
        .and_then(|c| c["alive"].as_array())
        .is_some_and(|a| a.iter().all(|v| v == false));
    reap_survivors(rec, &sleep_procs);
    s.srv.shutdown().await?;
    let was_listed = listed
        .as_ref()
        .ok()
        .and_then(|v| v["data"].as_array().map(|a| !a.is_empty()))
        .unwrap_or(false);
    Ok(
        if out.status == Some(TurnStatus::Interrupted) && !sleep_procs.is_empty() && stopped {
            Verdict::new(
                "PASS",
                format!(
                    "interrupted turn left the command running (listed as background terminal: {was_listed}); experimental thread/backgroundTerminals/clean stopped it"
                ),
            )
        } else {
            Verdict::new(
                "PARTIAL",
                format!(
                    "status {:?}, sleep seen {}, listed {was_listed}, stopped by clean {stopped}",
                    out.status,
                    !sleep_procs.is_empty()
                ),
            )
        },
    )
}
