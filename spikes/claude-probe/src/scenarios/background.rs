//! Фоновая команда (`run_in_background`): видна ли в потоке, что с ней при EOF/SIGTERM,
//! переживает ли она процесс CLI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use serde_json::{Value, json};

use super::{Ctx, TURN_TIMEOUT, result_summary};
use crate::broker::{self, PolicyAction};
use crate::config::Config;
use crate::process::ProcInfo;

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(cfg, "background", broker::deny_all("placeholder")).await?;
    ctx.broker.set_policy(Arc::new(|args: &Value| {
        let input = broker::requested_input(args);
        let cmd = input["command"].as_str().unwrap_or_default();
        if broker::requested_tool(args) == "Bash" && cmd.contains("bg.txt") {
            PolicyAction::Respond(broker::allow(&input))
        } else {
            PolicyAction::Respond(broker::deny(
                "background scenario: only the background command is allowed",
            ))
        }
    }));
    let mut s = ctx.spawn(&[], &[])?;

    ctx.step("turn: start a background Bash command and end the turn immediately");
    ctx.turns += 1;
    let out = s
        .turn(
            "Use the Bash tool with run_in_background set to true to run exactly this command: `sleep 40 && echo bg-finished > bg.txt`. \
             Do not wait for it and do not check on it. Immediately after starting it, reply with exactly the word STARTED.",
            TURN_TIMEOUT,
        )
        .await?;
    s.pump_for(Duration::from_secs(2)).await;
    let r = out.result.clone().unwrap_or_default();
    let bg_use = s
        .tools
        .uses()
        .iter()
        .find(|u| {
            u.input["command"]
                .as_str()
                .is_some_and(|c| c.contains("bg.txt"))
        })
        .cloned();
    let bg_result = bg_use
        .as_ref()
        .and_then(|u| s.tools.result_for(&u.id))
        .cloned();
    let tree =
        s.p.record_tree("after the turn that started the background command");
    let bg_sleep: Option<ProcInfo> = tree.iter().find(|p| p.comm == "sleep").cloned();
    let task_events: Vec<Value> = s
        .system
        .iter()
        .filter(|(_, v)| {
            v["subtype"]
                .as_str()
                .is_some_and(|st| st.starts_with("task") || st.contains("background"))
        })
        .map(|(t, v)| json!({"t_ms": t, "frame": v}))
        .collect();
    let observable = ctx.check(
        "background_visible_in_stream",
        bg_use
            .as_ref()
            .is_some_and(|u| u.input["run_in_background"] == true)
            && bg_result.is_some(),
        json!({
            "tool_use": bg_use.as_ref().map(|u| json!({"id": u.id, "input": u.input})),
            "tool_result": bg_result.map(|tr| json!({"is_error": tr.is_error, "text": tr.text})),
            "task_system_events": task_events,
            "result": result_summary(&r),
            "bg_sleep_process": bg_sleep.as_ref().map(ProcInfo::to_json),
        }),
    );

    ctx.step("close stdin (EOF) while the background command still runs; observe for 10s");
    let t_eof = ctx.rec.elapsed_ms();
    s.p.close_stdin();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && s.p.try_exit().is_none() {
        s.pump_for(Duration::from_millis(250)).await;
    }
    let exited_on_eof = s.p.try_exit();
    let bg_alive_after_eof = bg_sleep.as_ref().map(ProcInfo::still_alive);
    s.p.record_tree("10s after EOF");
    ctx.check(
        "eof_with_background_running",
        true,
        json!({"cli_exited_within_10s": exited_on_eof.map(|st| crate::process::exit_json(&st)),
               "bg_sleep_alive": bg_alive_after_eof, "ms_since_eof": ctx.rec.elapsed_ms() - t_eof,
               "events_after_eof": s.system.iter().filter(|(t, _)| *t >= t_eof).map(|(_, v)| v["subtype"].clone()).collect::<Vec<_>>()}),
    );

    let mut sigterm_note = Value::Null;
    if exited_on_eof.is_none() {
        ctx.step("CLI still waiting for background work: send SIGTERM (documented: kills running Bash trees)");
        let t_term = s.p.signal(Signal::SIGTERM);
        s.pump_for(Duration::from_secs(3)).await;
        let st = s.p.wait_exit(Duration::from_secs(15)).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        sigterm_note = json!({
            "exit": st.map(|x| crate::process::exit_json(&x)),
            "ms_after_sigterm": ctx.rec.elapsed_ms() - t_term,
            "bg_sleep_alive_after_sigterm": bg_sleep.as_ref().map(ProcInfo::still_alive),
        });
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bg_file = ctx.cwd.join("bg.txt").exists();
    let leftover = bg_sleep.as_ref().filter(|p| p.still_alive()).map(|p| {
        let killed = signal::kill(Pid::from_raw(p.pid), Signal::SIGKILL).is_ok();
        json!({"pid": p.pid, "killed_by_probe": killed})
    });
    ctx.check(
        "background_fate_on_exit",
        true,
        json!({"sigterm": sigterm_note, "bg_txt_created": bg_file, "leftover_cleanup": leftover}),
    );

    ctx.finish(
        if observable { "PASS" } else { "FAIL" },
        if observable {
            "background Bash command visible as tool_use(run_in_background)+tool_result; exit behaviour recorded"
        } else {
            "background command not observed; see check notes"
        },
    );
    Ok(())
}
