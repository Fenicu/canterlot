//! Доказательство приёма ввода: эхо `--replay-user-messages` с нашим UUID,
//! `user_message_uuid(s)` на первом кадре ответа и в result, запись в транскрипт;
//! повтор UUID, битая строка и неизвестный тип на stdin.

use std::time::Duration;

use serde_json::{Value, json};

use super::{Ctx, TURN_TIMEOUT, new_uuid, result_summary};
use crate::broker::deny_all;
use crate::config::Config;
use crate::protocol;

/// Каталог транскриптов CLI для cwd (документированное правило имени).
pub fn transcript_path(cwd: &std::path::Path, session_id: &str) -> std::path::PathBuf {
    let project: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::Path::new(&home)
        .join(".claude/projects")
        .join(project)
        .join(format!("{session_id}.jsonl"))
}

/// Сводка транскрипта без содержимого: тип, uuid и признаки каждой записи.
pub fn transcript_summary(path: &std::path::Path) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return json!({"path": path, "exists": false});
    };
    let entries: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| {
            let blocks: Vec<Value> = v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .map(|bs| bs.iter().map(|b| b["type"].clone()).collect())
                .unwrap_or_default();
            json!({"type": v["type"], "subtype": v["subtype"], "uuid": v["uuid"], "parentUuid": v["parentUuid"],
                   "content_blocks": blocks, "isMeta": v["isMeta"], "toolUseResult_present": v.get("toolUseResult").is_some()})
        })
        .collect();
    json!({"path": path, "exists": true, "lines": text.lines().count(), "entries": entries})
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(
        cfg,
        "delivery_ack",
        deny_all("delivery_ack: no tool calls expected"),
    )
    .await?;
    let mut s = ctx.spawn(&[], &[])?;

    ctx.step("input with our own UUID: collect every observable acknowledgement");
    ctx.turns += 1;
    let first_id = new_uuid();
    let first_msg = protocol::user_message(
        &first_id,
        "Reply with exactly the single word RECEIVED and nothing else.",
    );
    s.send_user_value(&first_id, &first_msg).await?;
    let r = s
        .wait_next_result(TURN_TIMEOUT)
        .await
        .map(|(_, r)| r)
        .unwrap_or_default();
    let out = super::TurnOutcome {
        uuid: first_id.clone(),
        written_ms: s.inputs.get(&first_id).map(|e| e.written_ms).unwrap_or(0),
        result: Some(r.clone()),
    };
    let replay = s.replay_matches(&out.uuid, &first_msg);
    let entry = s.inputs.get(&out.uuid).cloned();
    let echo_ok = replay && entry.as_ref().is_some_and(|e| e.echo_ms.is_some());
    let first_reply_ok = entry.as_ref().is_some_and(|e| e.first_reply_ms.is_some());
    let result_ok = protocol::answered_uuids(&r).contains(&out.uuid);
    let sid = s.session_id().unwrap_or_default();
    let tpath = transcript_path(&ctx.cwd, &sid);
    let transcript_has_uuid = std::fs::read_to_string(&tpath)
        .map(|t| t.contains(&out.uuid))
        .unwrap_or(false);
    ctx.check(
        "ack_chain",
        echo_ok && first_reply_ok && result_ok,
        json!({
            "delivery": s.delivery_note(&out.uuid),
            "replay_uuid_preserved": replay,
            "result": result_summary(&r),
            "transcript_contains_our_uuid": transcript_has_uuid,
            "transcript_path": tpath,
        }),
    );

    ctx.step("duplicate: second input reusing the same UUID with different text");
    ctx.turns += 1;
    let dup = protocol::user_message(
        &out.uuid,
        "Reply with exactly the single word DUPLICATE and nothing else.",
    );
    let results_before = s.results.len();
    let t_dup = s.p.send(&dup).await?;
    let dup_result = s.wait_next_result(Duration::from_secs(45)).await;
    s.pump_for(Duration::from_secs(2)).await;
    let replays_of_uuid = s.replay_count(&out.uuid);
    let transcript_text = std::fs::read_to_string(&tpath).unwrap_or_default();
    ctx.check(
        "duplicate_uuid_behaviour",
        true,
        json!({
            "written_ms": t_dup,
            "new_result": dup_result.as_ref().map(|(_, r)| result_summary(r)),
            "new_results_count": s.results.len() - results_before,
            "replays_with_this_uuid_total": replays_of_uuid,
            "text_after_dup": s.final_text_since(t_dup),
            "duplicate_text_in_transcript": transcript_text.contains("single word DUPLICATE"),
            "lifecycle_for_uuid": s.delivery_note(&out.uuid)["undocumented_command_lifecycle"],
        }),
    );

    ctx.step("unknown message type on stdin");
    let t_unknown =
        s.p.send(&json!({"type": "bogus_probe_type", "uuid": new_uuid()}))
            .await?;
    s.pump_for(Duration::from_secs(5)).await;
    let alive_after_unknown = s.p.try_exit().is_none();
    ctx.check(
        "unknown_type_line",
        true,
        json!({"written_ms": t_unknown, "process_alive_after_5s": alive_after_unknown,
               "frames_after": s.other.iter().filter(|(t, _)| *t >= t_unknown).map(|(_, v)| v["type"].clone()).collect::<Vec<_>>()}),
    );

    let mut recovered = None;
    if alive_after_unknown {
        ctx.step("valid input after the unknown line: does the session still work?");
        ctx.turns += 1;
        let after = s
            .turn(
                "Reply with exactly the single word AFTER and nothing else.",
                TURN_TIMEOUT,
            )
            .await?;
        let ok = after
            .result
            .as_ref()
            .is_some_and(|r| r["subtype"] == "success");
        recovered = Some(ok);
        ctx.check(
            "session_usable_after_unknown_line",
            ok,
            json!({"delivery": s.delivery_note(&after.uuid)}),
        );
    }

    ctx.step("malformed JSON line on stdin (last step: may end the process)");
    let t_bad = s.p.send_text("{\"type\":\"user\",\"message\":").await?;
    s.pump_for(Duration::from_secs(5)).await;
    let exit_after_bad = s.p.wait_exit(Duration::from_millis(100)).await;
    ctx.check(
        "malformed_line",
        true,
        json!({"written_ms": t_bad, "process_exited": exit_after_bad.is_some(),
               "exit": exit_after_bad.map(|st| crate::process::exit_json(&st)),
               "results_after": s.results.iter().filter(|(t, _)| *t >= t_bad).map(|(_, r)| result_summary(r)).collect::<Vec<_>>()}),
    );

    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.rec
        .note(json!({"exit_after_eof": status.map(|st| crate::process::exit_json(&st))}));
    ctx.rec
        .note(json!({"transcript": transcript_summary(&tpath)}));
    ctx.rec.note(json!({"stream_hygiene": s.hygiene_note()}));

    let ok = echo_ok && first_reply_ok && result_ok;
    let verdict = if ok { "PASS" } else { "FAIL" };
    ctx.finish(
        verdict,
        &format!(
            "documented ack chain (replay echo + user_message_uuid on first reply + result) for our UUID: {ok}; unknown type tolerated: {alive_after_unknown}; usable after it: {recovered:?}; malformed line ends process: {}",
            exit_after_bad.is_some()
        ),
    );
    Ok(())
}
