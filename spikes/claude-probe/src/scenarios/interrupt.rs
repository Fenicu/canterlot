//! Прерывание текущего хода: запрос, подтверждение и фактическая остановка.
//! A: SIGINT (документирован для `claude -p`), B: control_request `interrupt`
//! (UNDOCUMENTED форма вложенного запроса) с `cancel_queued`, C: прерывание при
//! ожидающем решении о разрешении и позднее решение после прерывания.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use serde_json::{Value, json};

use super::{Ctx, Session, TURN_TIMEOUT, new_uuid, result_summary};
use crate::broker::{self, PermissionSeen, PolicyAction};
use crate::config::Config;
use crate::process::ProcInfo;
use crate::protocol::{self, Kind};

#[derive(Clone, Copy, PartialEq)]
enum Mechanism {
    Sigint,
    ControlRequest,
}

/// Ждать запрос разрешения, параллельно обрабатывая поток CLI.
async fn wait_permission(
    ctx: &mut Ctx,
    s: &mut Session,
    timeout: Duration,
) -> Option<PermissionSeen> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(p) = ctx.perms.try_recv() {
            return Some(p);
        }
        s.pump_for(Duration::from_millis(150)).await;
    }
    None
}

/// Найти процесс `sleep` в дереве CLI (доказательство, что инструмент реально работает).
async fn wait_sleep_proc(s: &mut Session, timeout: Duration) -> Option<ProcInfo> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(p) = s.p.tree().into_iter().find(|p| p.comm == "sleep") {
            return Some(p);
        }
        s.pump_for(Duration::from_millis(150)).await;
    }
    None
}

struct Observed {
    request_ms: u64,
    ack_ms: Option<u64>,
    ack: Option<Value>,
    result_ms: Option<u64>,
    result: Option<Value>,
}

async fn interrupt_and_observe(
    ctx: &Ctx,
    s: &mut Session,
    mech: Mechanism,
    cancel_queued: bool,
) -> Observed {
    let request_id = format!("probe-interrupt-{}", new_uuid());
    let request_ms = match mech {
        Mechanism::Sigint => s.p.signal(Signal::SIGINT),
        Mechanism::ControlRequest => {
            s.p.send(&protocol::control_interrupt(&request_id, cancel_queued))
                .await
                .unwrap_or_else(|_| ctx.rec.elapsed_ms())
        }
    };
    let mut ack_ms = None;
    let mut ack = None;
    let got = s
        .pump_until(Duration::from_secs(30), |t, v| {
            if protocol::classify(v) == Kind::ControlResponse && ack_ms.is_none() {
                ack_ms = Some(t);
                ack = Some(v.clone());
            }
            protocol::classify(v) == Kind::Result
        })
        .await;
    Observed {
        request_ms,
        ack_ms,
        ack,
        result_ms: got.as_ref().map(|(t, _)| *t),
        result: got.map(|(_, v)| v),
    }
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(cfg, "interrupt", broker::deny_all("placeholder")).await?;
    ctx.broker.set_policy(Arc::new(|args: &Value| {
        let cmd = broker::requested_input(args)["command"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if broker::requested_tool(args) == "Bash" && cmd.starts_with("sleep ") {
            PolicyAction::Respond(broker::allow(&broker::requested_input(args)))
        } else if cmd.contains("held.txt") {
            PolicyAction::Hold
        } else {
            PolicyAction::Respond(broker::deny("interrupt scenario: unexpected action"))
        }
    }));

    let mut s = ctx.spawn(&[], &[])?;
    let mut verdicts: Vec<(&str, bool)> = Vec::new();

    // ---------- A: SIGINT ----------
    ctx.step("A: SIGINT while an allowed `sleep 20` Bash call is running");
    ctx.turns += 1;
    let (a_uuid, _) = s
        .send_user("Run the bash command `sleep 20` in the foreground (not in the background), then reply with exactly the word SLEPT.")
        .await?;
    // `sleep` может не требовать разрешения: ждём сам процесс, а запрос (если был) читаем без ожидания.
    let sleep_a = wait_sleep_proc(&mut s, Duration::from_secs(60)).await;
    let perm = ctx.perms.try_recv().ok();
    s.pump_for(Duration::from_millis(1500)).await;
    s.p.record_tree("A: before SIGINT");
    let obs = interrupt_and_observe(&ctx, &mut s, Mechanism::Sigint, false).await;
    s.pump_for(Duration::from_secs(2)).await;
    let alive_a = s.p.try_exit().is_none();
    s.p.record_tree("A: 2s after SIGINT result");
    let sleep_a_alive = sleep_a.as_ref().map(ProcInfo::still_alive);
    let sleep_use = s
        .tools
        .uses()
        .iter()
        .find(|u| {
            u.input["command"]
                .as_str()
                .is_some_and(|c| c.starts_with("sleep 20"))
        })
        .cloned();
    let sleep_result = sleep_use
        .as_ref()
        .and_then(|u| s.tools.result_for(&u.id))
        .cloned();
    let a_stopped = obs.result.is_some();
    ctx.check(
        "A_sigint",
        a_stopped,
        json!({
            "permission_request_seen": perm.as_ref().map(|p| json!({"arguments": p.arguments, "decision": p.decision})),
            "sleep_process_seen": sleep_a.as_ref().map(ProcInfo::to_json),
            "request_ms": obs.request_ms,
            "result_ms": obs.result_ms,
            "latency_request_to_result_ms": obs.result_ms.map(|t| t - obs.request_ms),
            "result": obs.result.as_ref().map(result_summary),
            "tool_result": sleep_result.map(|tr| json!({"is_error": tr.is_error, "text": tr.text})),
            "cli_alive_after": alive_a,
            "exit": s.p.try_exit().map(|st| crate::process::exit_json(&st)),
            "sleep_child_alive_after": sleep_a_alive,
            "delivery": s.delivery_note(&a_uuid),
        }),
    );
    let mut a_next_ok = false;
    if alive_a {
        ctx.step("A: next input in the same process after SIGINT");
        ctx.turns += 1;
        let next = s
            .turn(
                "Reply with exactly the single word AFTER-SIGINT and nothing else.",
                TURN_TIMEOUT,
            )
            .await?;
        a_next_ok = next
            .result
            .as_ref()
            .is_some_and(|r| r["subtype"] == "success");
        ctx.check(
            "A_next_input_after_sigint",
            a_next_ok,
            json!({"result": next.result.as_ref().map(result_summary)}),
        );
    }
    verdicts.push(("A_sigint", a_stopped && sleep_a_alive != Some(true)));

    if s.p.try_exit().is_some() {
        ctx.step("process ended after SIGINT: start a fresh process for B and C");
        s = ctx.spawn(&[], &[])?;
    }

    // ---------- B: control_request interrupt + cancel_queued ----------
    ctx.step("B: UNDOCUMENTED-shape control_request interrupt (cancel_queued=true) while `sleep 21` runs and one more input is queued");
    ctx.turns += 1;
    let (b_uuid, _) = s
        .send_user("Run the bash command `sleep 21` in the foreground (not in the background), then reply with exactly the word SLEPT.")
        .await?;
    let sleep_b = wait_sleep_proc(&mut s, Duration::from_secs(60)).await;
    let perm_b = ctx.perms.try_recv().ok();
    let (q_uuid, q_t) = s
        .send_user("Reply with exactly the single word QUEUED and nothing else.")
        .await?;
    s.pump_for(Duration::from_millis(1500)).await;
    s.p.record_tree("B: before control interrupt");
    let obs_b = interrupt_and_observe(&ctx, &mut s, Mechanism::ControlRequest, true).await;
    s.pump_for(Duration::from_secs(4)).await;
    let alive_b = s.p.try_exit().is_none();
    let sleep_b_alive = sleep_b.as_ref().map(ProcInfo::still_alive);
    let q_ran = s
        .results
        .iter()
        .any(|(t, r)| *t >= q_t && protocol::answered_uuids(r).contains(&q_uuid));
    let receipt = obs_b.ack.as_ref().map(|a| a["response"].clone());
    let b_stopped = obs_b.ack.is_some() && obs_b.result.is_some();
    ctx.check(
        "B_control_interrupt",
        b_stopped,
        json!({
            "permission_request_seen": perm_b.as_ref().map(|p| json!({"arguments": p.arguments, "decision": p.decision})),
            "sleep_process_seen": sleep_b.as_ref().map(ProcInfo::to_json),
            "request_ms": obs_b.request_ms,
            "ack_ms": obs_b.ack_ms,
            "ack": obs_b.ack,
            "latency_request_to_ack_ms": obs_b.ack_ms.map(|t| t - obs_b.request_ms),
            "result_ms": obs_b.result_ms,
            "latency_request_to_result_ms": obs_b.result_ms.map(|t| t - obs_b.request_ms),
            "result": obs_b.result.as_ref().map(result_summary),
            "queued_input_uuid": q_uuid,
            "queued_input_listed_as_cancelled": receipt.as_ref().map(|r| r.to_string().contains(&q_uuid)),
            "queued_input_ran": q_ran,
            "queued_delivery": s.delivery_note(&q_uuid),
            "cli_alive_after": alive_b,
            "sleep_child_alive_after": sleep_b_alive,
            "delivery": s.delivery_note(&b_uuid),
        }),
    );
    let mut b_next_ok = false;
    if alive_b {
        ctx.step("B: next input in the same process after control interrupt");
        ctx.turns += 1;
        let next = s
            .turn(
                "Reply with exactly the single word AFTER-CONTROL and nothing else.",
                TURN_TIMEOUT,
            )
            .await?;
        b_next_ok = next
            .result
            .as_ref()
            .is_some_and(|r| r["subtype"] == "success");
        ctx.check(
            "B_next_input_after_control_interrupt",
            b_next_ok,
            json!({"result": next.result.as_ref().map(result_summary)}),
        );
    }
    verdicts.push(("B_control", b_stopped && b_next_ok));

    if s.p.try_exit().is_some() {
        s = ctx.spawn(&[], &[])?;
    }

    // ---------- C: interrupt while a permission request is pending ----------
    let mech_c = if alive_a {
        Mechanism::Sigint
    } else {
        Mechanism::ControlRequest
    };
    ctx.step(&format!(
        "C: interrupt ({}) while the permission request for `touch held.txt` is held unanswered; then answer it late",
        if mech_c == Mechanism::Sigint { "SIGINT" } else { "control_request" }
    ));
    ctx.turns += 1;
    let (c_uuid, _) = s
        .send_user("Run the bash command `touch held.txt`, then reply with exactly the word DONE.")
        .await?;
    let held = wait_permission(&mut ctx, &mut s, TURN_TIMEOUT).await;
    s.pump_for(Duration::from_millis(1000)).await;
    let obs_c = interrupt_and_observe(&ctx, &mut s, mech_c, false).await;
    s.pump_for(Duration::from_secs(2)).await;
    let late = match &held {
        Some(p) if p.decision.is_none() => {
            let input = broker::requested_input(&p.arguments);
            Some(ctx.broker.release(p.call_id, broker::allow(&input)).await)
        }
        _ => None,
    };
    s.pump_for(Duration::from_secs(3)).await;
    let held_file = ctx.cwd.join("held.txt").exists();
    let alive_c = s.p.try_exit().is_none();
    ctx.check(
        "C_interrupt_while_permission_pending",
        obs_c.result.is_some() && !held_file,
        json!({
            "held_request": held.as_ref().map(|p| &p.arguments),
            "request_ms": obs_c.request_ms,
            "ack": obs_c.ack,
            "result_ms": obs_c.result_ms,
            "latency_request_to_result_ms": obs_c.result_ms.map(|t| t - obs_c.request_ms),
            "result": obs_c.result.as_ref().map(result_summary),
            "late_allow_written_to_mcp": late,
            "held_txt_created_by_late_allow": held_file,
            "cli_alive_after": alive_c,
            "delivery": s.delivery_note(&c_uuid),
            "see_also": "mcp_trace notes for notifications/cancelled",
        }),
    );
    let mut c_next_ok = None;
    if alive_c {
        ctx.step("C: next input after the late decision");
        ctx.turns += 1;
        let next = s
            .turn(
                "Reply with exactly the single word AFTER-HELD and nothing else.",
                TURN_TIMEOUT,
            )
            .await?;
        let ok = next
            .result
            .as_ref()
            .is_some_and(|r| r["subtype"] == "success");
        c_next_ok = Some(ok);
        ctx.check(
            "C_next_input_after_late_decision",
            ok && !ctx.cwd.join("held.txt").exists(),
            json!({"result": next.result.as_ref().map(result_summary), "held_txt_exists": ctx.cwd.join("held.txt").exists()}),
        );
    }
    verdicts.push(("C_pending_permission", obs_c.result.is_some() && !held_file));

    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.rec.note(json!({"exit_after_eof": status.map(|st| crate::process::exit_json(&st)), "subverdicts": verdicts.iter().map(|(n, ok)| json!({"case": n, "ok": ok})).collect::<Vec<_>>(), "c_next_ok": c_next_ok}));

    let a_ok = verdicts.iter().any(|(n, ok)| *n == "A_sigint" && *ok);
    let all_ok = verdicts.iter().all(|(_, ok)| *ok);
    let (verdict, reason) = if all_ok && alive_a && a_next_ok {
        (
            "PASS",
            "documented SIGINT ends the running turn and kills its Bash child while the process stays usable; control interrupt and pending-permission cases also stop the turn",
        )
    } else if a_ok || all_ok {
        (
            "PARTIAL",
            "turn can be stopped, but some sub-case relies on an undocumented mechanism or lost the process; see checks",
        )
    } else {
        ("FAIL", "see check notes")
    };
    ctx.finish(verdict, reason);
    Ok(())
}
