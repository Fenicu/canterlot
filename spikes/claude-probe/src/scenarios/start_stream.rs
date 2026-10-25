//! Запуск долгоживущего stream-json процесса: init и изоляция, native session ID,
//! частичные текстовые дельты, финальное сообщение, итог хода, поведение на EOF.

use std::time::Duration;

use serde_json::json;

use super::{Ctx, TURN_TIMEOUT, result_summary};
use crate::broker::deny_all;
use crate::config::Config;
use crate::protocol;

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(
        cfg,
        "start_stream",
        deny_all("start_stream: no tool calls expected"),
    )
    .await?;
    // --include-hook-events: доказать, что пользовательские хуки не срабатывают.
    // --session-id: native ID назначается воркером до запуска (документированный флаг).
    let preassigned = super::new_uuid();
    let mut s = ctx.spawn(
        &["--include-hook-events", "--session-id", &preassigned],
        &[],
    )?;

    ctx.step("turn 1: streamed reply");
    ctx.turns += 1;
    let out = s
        .turn(
            "Count from 1 to 15 separated by single spaces. Output only the numbers.",
            TURN_TIMEOUT,
        )
        .await?;
    let result = out.result.clone().unwrap_or_default();

    let init = s.init.clone().unwrap_or_default();
    let isolation = s.isolation_ok() == Some(true);
    ctx.check("isolation", isolation, json!({"see": "isolation note"}));
    let init_sid = protocol::session_id(&init).map(str::to_owned);
    let result_sid = protocol::session_id(&result).map(str::to_owned);
    let sid_ok = ctx.check(
        "native_session_id",
        init_sid.as_deref() == Some(preassigned.as_str()) && init_sid == result_sid,
        json!({"preassigned_session_id": preassigned, "init_session_id": init_sid, "result_session_id": result_sid,
               "transcript_exists": super::delivery_ack::transcript_path(&ctx.cwd, &preassigned).exists(),
               "init_claude_code_version": init["claude_code_version"], "init_model": init["model"],
               "init_permissionMode": init["permissionMode"], "init_capabilities": init["capabilities"]}),
    );
    let streamed = s.text_since(out.written_ms);
    let final_text = s.final_text_since(out.written_ms);
    let deltas = s
        .deltas
        .iter()
        .filter(|(t, _)| *t >= out.written_ms)
        .count();
    let first_delta_ms = s
        .deltas
        .iter()
        .find(|(t, _)| *t >= out.written_ms)
        .map(|(t, _)| t - out.written_ms);
    let stream_ok = ctx.check(
        "partial_text_deltas",
        deltas >= 2 && streamed.trim() == final_text.trim() && !final_text.is_empty(),
        json!({"delta_count": deltas, "first_delta_after_write_ms": first_delta_ms,
               "concatenated_deltas": streamed, "final_assistant_text": final_text}),
    );
    let result_ok = ctx.check(
        "turn_result",
        result["subtype"] == "success" && result["is_error"] == false,
        result_summary(&result),
    );
    let hook_events: Vec<_> = s
        .system
        .iter()
        .filter(|(_, v)| {
            v["subtype"]
                .as_str()
                .is_some_and(|st| st.starts_with("hook_"))
        })
        .map(|(_, v)| v["subtype"].clone())
        .collect();
    let no_hooks = ctx.check(
        "no_hook_events",
        hook_events.is_empty(),
        json!({"hook_events": hook_events}),
    );
    ctx.rec
        .note(json!({"delivery": s.delivery_note(&out.uuid)}));
    ctx.rec.note(json!({"system_messages_seen": s.system.iter().map(|(_, v)| v["subtype"].clone()).collect::<Vec<_>>(),
                        "other_types_seen": s.other.iter().map(|(_, v)| v["type"].clone()).collect::<Vec<_>>()}));

    ctx.step("close stdin after the turn: observe clean exit");
    let t_close = ctx.rec.elapsed_ms();
    s.p.close_stdin();
    s.pump_for(Duration::from_secs(10)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.check(
        "eof_exit",
        status.is_some_and(|st| st.success()),
        json!({"exit": status.map(|st| crate::process::exit_json(&st)), "ms_after_eof": ctx.rec.elapsed_ms() - t_close}),
    );

    let ok = isolation && sid_ok && stream_ok && result_ok && no_hooks;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "isolated start, native session id from init, text deltas, final assistant message and success result observed"
        } else {
            "one of the checks failed, see check notes"
        },
    );
    Ok(())
}
