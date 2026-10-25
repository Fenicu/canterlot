//! Возобновление: `--resume <session_id>` после штатного завершения и после SIGKILL
//! посреди хода; состояние истории и прерванного ввода, судьба дочерних процессов.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use serde_json::{Value, json};

use super::delivery_ack::{transcript_path, transcript_summary};
use super::{Ctx, Session, TURN_TIMEOUT, result_summary};
use crate::broker::{self, PolicyAction};
use crate::config::Config;
use crate::process::ProcInfo;

const CODE_WORD: &str = "PELICAN-SEVEN";

async fn wait_proc(s: &mut Session, comm: &str, timeout: Duration) -> Option<ProcInfo> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(p) = s.p.tree().into_iter().find(|p| p.comm == comm) {
            return Some(p);
        }
        s.pump_for(Duration::from_millis(150)).await;
    }
    None
}

async fn close(ctx: &Ctx, s: &mut Session, label: &str) -> Value {
    let t = ctx.rec.elapsed_ms();
    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let st = s.p.wait_exit(Duration::from_secs(20)).await;
    json!({"label": label, "exit": st.map(|x| crate::process::exit_json(&x)), "ms_after_eof": ctx.rec.elapsed_ms() - t})
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(cfg, "resume", broker::deny_all("placeholder")).await?;
    ctx.broker.set_policy(Arc::new(|args: &Value| {
        let cmd = broker::requested_input(args)["command"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if cmd.trim().starts_with("sleep ") {
            PolicyAction::Respond(broker::allow(&broker::requested_input(args)))
        } else {
            PolicyAction::Respond(broker::deny("resume scenario: unexpected action"))
        }
    }));

    // ---------- 1: штатное завершение и resume ----------
    ctx.step("P1: new session, give a code word, then close stdin");
    let mut p1 = ctx.spawn(&[], &[])?;
    ctx.turns += 1;
    let t1 = p1
        .turn(&format!("The code word is {CODE_WORD}. Remember it. Reply with exactly the single word NOTED."), TURN_TIMEOUT)
        .await?;
    let sid1 = p1.session_id().unwrap_or_default();
    let exit1 = close(&ctx, &mut p1, "P1").await;
    ctx.check(
        "p1_clean_exit",
        exit1["exit"]["code"] == 0,
        json!({"session_id": sid1, "exit": exit1, "result": t1.result.as_ref().map(result_summary)}),
    );

    ctx.step("P2: --resume <session_id>, ask for the code word");
    let mut p2 = ctx.spawn(&["--resume", &sid1], &[])?;
    ctx.turns += 1;
    let t2 = p2
        .turn(
            "What is the code word I gave you earlier? Reply with only the code word.",
            TURN_TIMEOUT,
        )
        .await?;
    let answer = t2
        .result
        .as_ref()
        .and_then(|r| r["result"].as_str())
        .unwrap_or_default()
        .to_owned();
    let sid2 = p2.session_id().unwrap_or_default();
    let frames_before_input = p2
        .replays
        .iter()
        .filter(|(t, _)| *t < t2.written_ms)
        .count();
    let continuity = ctx.check(
        "resume_context_continuity",
        answer.contains(CODE_WORD),
        json!({"answer": answer, "resumed_session_id": sid2, "same_session_id": sid1 == sid2,
               "history_replayed_on_resume": frames_before_input,
               "replays_seen_total": p2.replays.len(),
               "result": t2.result.as_ref().map(result_summary)}),
    );
    let exit2 = close(&ctx, &mut p2, "P2").await;
    ctx.rec.note(json!({"p2_exit": exit2}));

    // ---------- 2: SIGKILL посреди хода ----------
    // `sleep 30` CLI блокирует встроенным правилом («standalone sleep»), `sleep 20` проходит.
    ctx.step("P3: new session, start `sleep 20`, SIGKILL the CLI process (pid only) mid-tool");
    let mut p3 = ctx.spawn(&[], &[])?;
    ctx.turns += 1;
    let (u3, _) = p3
        .send_user("Run the bash command `sleep 20` in the foreground (not in the background), then reply with exactly the word WOKE.")
        .await?;
    let sleep3 = wait_proc(&mut p3, "sleep", Duration::from_secs(60)).await;
    anyhow::ensure!(
        sleep3.is_some(),
        "sleep did not start; SIGKILL mid-tool cannot be tested"
    );
    p3.pump_for(Duration::from_millis(1000)).await;
    let sid3 = p3.session_id().unwrap_or_default();
    let before = p3.p.record_tree("P3: before SIGKILL");
    let t_kill = p3.p.signal(Signal::SIGKILL);
    let st3 = p3.p.wait_exit(Duration::from_secs(10)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let survivors: Vec<ProcInfo> = before
        .iter()
        .filter(|p| p.pid != p3.p.pid && p.still_alive())
        .cloned()
        .collect();
    let mcp_survived = survivors
        .iter()
        .any(|p| p.cmdline.contains("mcp-permission"));
    let sleep_survived = sleep3.as_ref().is_some_and(ProcInfo::still_alive);
    ctx.check(
        "sigkill_orphans",
        true,
        json!({
            "kill_ms": t_kill,
            "exit": st3.map(|x| crate::process::exit_json(&x)),
            "sleep_seen": sleep3.as_ref().map(ProcInfo::to_json),
            "survivors_after_500ms": survivors.iter().map(ProcInfo::to_json).collect::<Vec<_>>(),
            "sleep_survived": sleep_survived,
            "permission_mcp_server_survived": mcp_survived,
            "delivery_of_interrupted_input": p3.delivery_note(&u3),
        }),
    );
    // Уборка: только процессы, чья идентичность (pid + start_ticks) совпадает со снимком.
    let mut cleaned = Vec::new();
    for p in &survivors {
        if p.still_alive() {
            let ok = signal::kill(Pid::from_raw(p.pid), Signal::SIGKILL).is_ok();
            cleaned.push(json!({"pid": p.pid, "comm": p.comm, "killed": ok}));
        }
    }
    ctx.rec
        .note(json!({"cleanup_of_verified_orphans": cleaned}));
    let tpath3 = transcript_path(&ctx.cwd, &sid3);
    let transcript_text = std::fs::read_to_string(&tpath3).unwrap_or_default();
    ctx.rec.note(json!({"transcript_after_sigkill": transcript_summary(&tpath3),
                        "transcript_contains_interrupted_input_uuid": transcript_text.contains(&u3)}));

    ctx.step("P4: --resume the killed session, send a new input");
    let mut p4 = ctx.spawn(&["--resume", &sid3], &[])?;
    ctx.turns += 1;
    let t4 = p4
        .turn(
            "Do not run any tools. Reply with exactly the single word RESUMED, then on a second line say in a few words what happened to the sleep command you started before.",
            TURN_TIMEOUT,
        )
        .await?;
    let r4 = t4.result.clone().unwrap_or_default();
    let resumed_ok = ctx.check(
        "resume_after_sigkill",
        r4["subtype"] == "success" && p4.session_id().as_deref() == Some(sid3.as_str()),
        json!({"result": result_summary(&r4), "session_id": p4.session_id(), "expected_session_id": sid3,
               "replays_on_resume": p4.replays.iter().map(|(t, v)| json!({"t_ms": t, "uuid": v["uuid"], "content": v["message"]["content"]})).collect::<Vec<_>>(),
               "system_messages": p4.system.iter().map(|(_, v)| v["subtype"].clone()).collect::<Vec<_>>()}),
    );
    let exit4 = close(&ctx, &mut p4, "P4").await;
    ctx.rec
        .note(json!({"p4_exit": exit4, "transcript_after_resume": transcript_summary(&tpath3)}));

    let ok = continuity && resumed_ok;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "--resume restores context after clean exit and after SIGKILL mid-turn; same native session id; see orphan and transcript notes"
        } else {
            "see check notes"
        },
    );
    Ok(())
}
