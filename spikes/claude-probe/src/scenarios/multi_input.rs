//! Несколько последовательных вводов в одном процессе (AT-G-01) и ввод,
//! записанный во время потоковой генерации предыдущего хода.

use std::time::Duration;

use serde_json::{Value, json};

use super::{Ctx, TURN_TIMEOUT, result_summary};
use crate::broker::deny_all;
use crate::config::Config;
use crate::protocol::{self, Kind};

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(
        cfg,
        "multi_input",
        deny_all("multi_input: no tool calls expected"),
    )
    .await?;
    let mut s = ctx.spawn(&[], &[])?;

    let mut sequential_ok = true;
    let mut result_order: Vec<String> = Vec::new();
    for word in ["ALPHA", "BRAVO", "CHARLIE"] {
        ctx.step(&format!(
            "sequential input {word}: wait for the previous result before writing"
        ));
        ctx.turns += 1;
        let out = s
            .turn(
                &format!("Reply with exactly the single word {word} and nothing else."),
                TURN_TIMEOUT,
            )
            .await?;
        let r = out.result.clone().unwrap_or_default();
        let answered = protocol::answered_uuids(&r).contains(&out.uuid);
        let text = r["result"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .trim_end_matches('.')
            .to_owned();
        let ok = answered && r["subtype"] == "success" && text == word;
        sequential_ok &= ctx.check(
            &format!("sequential_{word}"),
            ok,
            json!({"delivery": s.delivery_note(&out.uuid), "result": result_summary(&r),
                   "streamed_text": s.text_since(out.written_ms)}),
        );
        result_order.push(text);
    }
    let same_session = {
        let ids: Vec<Option<&str>> = s
            .results
            .iter()
            .map(|(_, r)| protocol::session_id(r))
            .collect();
        ids.windows(2).all(|w| w[0] == w[1])
    };
    ctx.check(
        "same_process_same_session",
        same_session && s.p.try_exit().is_none(),
        json!({"pid": s.p.pid, "result_order": result_order, "session_id": s.session_id()}),
    );

    ctx.step("concurrent: write input E while turn D is still streaming (default priority, no `priority` field)");
    ctx.turns += 2;
    let results_before = s.results.len();
    let (d_uuid, d_t) = s
        .send_user("Count from 1 to 60, one number per line. Output only the numbers.")
        .await?;
    let streaming = s
        .pump_until(TURN_TIMEOUT, |_, v| protocol::text_delta(v).is_some())
        .await;
    let (e_uuid, e_t) = s
        .send_user("Reply with exactly the single word ECHO and nothing else.")
        .await?;
    ctx.rec.note(json!({"concurrent_write": {"d_uuid": d_uuid, "d_written_ms": d_t,
        "d_first_delta_ms": streaming.as_ref().map(|(t, _)| t), "e_uuid": e_uuid, "e_written_ms": e_t}}));

    let mut both = false;
    for _ in 0..4 {
        if s.wait_next_result(TURN_TIMEOUT).await.is_none() {
            break;
        }
        let answered: Vec<String> = s.results[results_before..]
            .iter()
            .flat_map(|(_, r)| protocol::answered_uuids(r))
            .collect();
        if answered.contains(&d_uuid) && answered.contains(&e_uuid) {
            both = true;
            break;
        }
    }
    s.pump_for(Duration::from_secs(2)).await;
    let new_results: Vec<Value> = s.results[results_before..]
        .iter()
        .map(|(t, r)| json!({"t_ms": t, "summary": result_summary(r)}))
        .collect();
    let d_result_t = s.inputs.get(&d_uuid).and_then(|e| e.result_ms);
    let e_echo_t = s.inputs.get(&e_uuid).and_then(|e| e.echo_ms);
    let merged = s.results[results_before..].iter().any(|(_, r)| {
        let a = protocol::answered_uuids(r);
        a.contains(&d_uuid) && a.contains(&e_uuid)
    });
    let d_text = s.results[results_before..]
        .iter()
        .find(|(_, r)| protocol::answered_uuids(r).contains(&d_uuid))
        .and_then(|(_, r)| r["result"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let d_complete = (1..=60).all(|n| d_text.lines().any(|l| l.trim() == n.to_string()));
    ctx.check(
        "concurrent_input_behaviour",
        both,
        json!({
            "both_inputs_answered": both,
            "merged_into_one_turn": merged,
            "results_after_d": new_results,
            "d_delivery": s.delivery_note(&d_uuid),
            "e_delivery": s.delivery_note(&e_uuid),
            "e_echo_before_d_result": match (e_echo_t, d_result_t) { (Some(e), Some(d)) => Some(e < d), _ => None },
            "d_text_complete_1_to_60": d_complete,
            "system_status_frames": s.system.iter().filter(|(t, v)| *t >= d_t && v["subtype"] == "status").map(|(t, v)| json!({"t_ms": t, "status": v["status"], "user_message_uuids": v["user_message_uuids"]})).collect::<Vec<_>>(),
        }),
    );

    ctx.step("close stdin: clean exit");
    s.p.close_stdin();
    s.pump_for(Duration::from_secs(5)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.check(
        "eof_exit",
        status.is_some(),
        json!({"exit": status.map(|st| crate::process::exit_json(&st))}),
    );
    let leftovers = s
        .other
        .iter()
        .filter(|(_, v)| protocol::classify(v) != Kind::Other("rate_limit_event".into()))
        .count();
    ctx.rec.note(json!({"other_frames_count": leftovers}));

    let ok = sequential_ok && same_session && both;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "three sequential inputs answered in order in one process; input written mid-stream was queued and answered"
        } else {
            "see check notes"
        },
    );
    Ok(())
}
