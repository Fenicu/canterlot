//! Отказ внешнего сервиса без расхода квоты: недоступный провайдер и неверная модель.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{TURN_WAIT, Verdict};
use crate::appserver::Event;
use crate::driver::{
    Action, RPC_WAIT, Session, SessionOptions, TurnState, drive_turn, simple_turn, text_input,
};
use crate::evidence::Recorder;
use crate::isolation;
use crate::protocol::TurnStatus;

/// Сколько ждать терминального состояния, прежде чем прервать ход вручную.
const UNREACHABLE_GIVE_UP: Duration = Duration::from_secs(150);

pub async fn service_failure(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    rec.step(
        "part A: model provider endpoint unreachable (127.0.0.1:9, nothing listens; no auth sent)",
    );
    let dead = |kv: &str| ["-c".to_string(), kv.to_string()];
    let mut extra = Vec::new();
    for kv in [
        "model_provider=\"canterlot_dead\"",
        "model_providers.canterlot_dead.name=\"canterlot unreachable endpoint\"",
        "model_providers.canterlot_dead.base_url=\"http://127.0.0.1:9/v1\"",
        "model_providers.canterlot_dead.requires_openai_auth=false",
        "model_providers.canterlot_dead.supports_websockets=false",
    ] {
        extra.extend(dead(kv));
    }
    let mut a = Session::open(
        rec,
        scratch,
        SessionOptions {
            extra_args: extra,
            ..SessionOptions::default()
        },
    )
    .await?;
    let ta = a.start_thread(true).await?;
    let mut timeline: Vec<Value> = Vec::new();
    let mut interrupted = false;
    let out_a = {
        let mut hook = |ev: &Event, st: &TurnState| -> Vec<Action> {
            let at = st.started.elapsed();
            if let Event::Notification { method, params } = ev
                && (method == "error"
                    || method == "turn/completed"
                    || method == "thread/status/changed"
                    || method == "warning")
            {
                timeline.push(json!({ "at_ms": at.as_millis() as u64, "method": method,
                        "willRetry": params.get("willRetry"), "error": params.get("error"),
                        "status": params.get("status").or_else(|| params["turn"].get("status")) }));
            }
            if !interrupted && at > UNREACHABLE_GIVE_UP {
                interrupted = true;
                return vec![Action::Interrupt];
            }
            Vec::new()
        };
        drive_turn(
            &mut a.srv,
            &ta.thread.id,
            json!({ "input": text_input("Reply OK.") }),
            &mut hook,
            TURN_WAIT + Duration::from_secs(60),
        )
        .await?
    };
    rec.note(json!({ "unreachable": { "timeline": timeline, "final_status": format!("{:?}", out_a.status),
        "turn_error": out_a.error.as_ref().map(|e| json!({ "message": e.message, "codexErrorInfo": e.codex_error_info, "additionalDetails": e.additional_details })),
        "error_notifications": out_a.errors.len(),
        "retrying_errors": out_a.errors.iter().filter(|e| e.will_retry).count(),
        "client_had_to_interrupt": interrupted, "completed_at_ms": out_a.t_completed_ms } }));
    a.srv.shutdown().await?;

    rec.step("part B: invalid model name on the real provider (rejected request, no generation)");
    let mut b = Session::open(rec, scratch, SessionOptions::default()).await?;
    let mut params = isolation::thread_start_params(scratch, true);
    params["model"] = json!("canterlot-no-such-model");
    let tb = b.srv.call("thread/start", params, RPC_WAIT).await?;
    rec.note(json!({ "thread_start_with_invalid_model": match &tb {
        Ok(v) => json!({ "ok": true, "threadId": v["thread"]["id"], "model": v["model"] }),
        Err(e) => json!({ "error": e }) } }));
    let mut b_status = None;
    let mut b_error = Value::Null;
    if let Ok(v) = &tb {
        let tid = v["thread"]["id"].as_str().unwrap_or_default().to_string();
        let out_b = simple_turn(
            &mut b.srv,
            &tid,
            "Reply OK.",
            None,
            Duration::from_secs(120),
        )
        .await?;
        b_status = out_b.status.clone();
        b_error = json!({ "turn_error": out_b.error.as_ref().map(|e| json!({ "message": e.message, "codexErrorInfo": e.codex_error_info })),
            "error_notifications": out_b.errors.iter().map(|e| json!({ "message": e.error.message, "willRetry": e.will_retry, "codexErrorInfo": e.error.codex_error_info })).collect::<Vec<_>>(),
            "completed_at_ms": out_b.t_completed_ms, "start_rejected": out_b.start_error.as_ref().map(|e| json!(e)) });
    }
    rec.note(json!({ "invalid_model": b_error, "status": format!("{b_status:?}") }));
    b.srv.shutdown().await?;

    let a_surfaced = !out_a.errors.is_empty() || out_a.status == Some(TurnStatus::Failed);
    let b_surfaced = tb.is_err() || b_status == Some(TurnStatus::Failed);
    Ok(if a_surfaced && b_surfaced {
        Verdict::new(
            "PASS",
            format!(
                "unreachable endpoint surfaced as {} error notification(s) ({} with willRetry) and final status {:?}{}; invalid model surfaced as {}",
                out_a.errors.len(),
                out_a.errors.iter().filter(|e| e.will_retry).count(),
                out_a.status,
                if interrupted {
                    " (client interrupt needed)"
                } else {
                    ""
                },
                if tb.is_err() {
                    "thread/start error"
                } else {
                    "failed turn"
                }
            ),
        )
    } else {
        Verdict::new(
            "PARTIAL",
            format!(
                "failure surfacing incomplete: unreachable={a_surfaced}, invalid_model={b_surfaced}"
            ),
        )
    })
}
