//! Отказ внешнего сервиса без расхода квоты: несуществующая модель и
//! недоступный прокси (с повторами по умолчанию и с `CLAUDE_CODE_MAX_RETRIES=1`).

use std::time::Duration;

use nix::sys::signal::Signal;
use serde_json::{Value, json};

use super::{Ctx, Session, result_summary};
use crate::broker::deny_all;
use crate::config::Config;
use crate::protocol::{self, Kind};

const DEAD_PROXY: &str = "http://127.0.0.1:9";

struct Outcome {
    written_ms: u64,
    result: Option<(u64, Value)>,
}

async fn attempt(s: &mut Session, window: Duration) -> anyhow::Result<Outcome> {
    let (_, written_ms) = s
        .send_user("Reply with exactly the single word OK and nothing else.")
        .await?;
    let result = s
        .pump_until(window, |_, v| protocol::classify(v) == Kind::Result)
        .await;
    Ok(Outcome { written_ms, result })
}

fn summarize(ctx: &Ctx, s: &Session, o: &Outcome) -> Value {
    let retries: Vec<Value> = s
        .system
        .iter()
        .filter(|(t, v)| *t >= o.written_ms && v["subtype"] == "api_retry")
        .map(|(t, v)| json!({"t_ms": t, "attempt": v["attempt"], "max_retries": v["max_retries"],
                             "retry_delay_ms": v["retry_delay_ms"], "error_status": v["error_status"],
                             "error": v["error"], "no_response": v["no_response"]}))
        .collect();
    let assistant_errors: Vec<Value> = s
        .assistants
        .iter()
        .filter(|(t, v)| *t >= o.written_ms && v.get("error").is_some())
        .map(|(t, v)| json!({"t_ms": t, "error": v["error"], "text": protocol::assistant_text(v)}))
        .collect();
    json!({
        "written_ms": o.written_ms,
        "api_retry_events": retries,
        "assistant_error_messages": assistant_errors,
        "result_after_ms": o.result.as_ref().map(|(t, _)| t - o.written_ms),
        "result": o.result.as_ref().map(|(_, r)| result_summary(r)),
        "stderr_lines_seen": "see err entries",
        "now_ms": ctx.rec.elapsed_ms(),
    })
}

async fn finish_proc(ctx: &Ctx, s: &mut Session) -> Value {
    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let mut st = s.p.wait_exit(Duration::from_secs(10)).await;
    if st.is_none() {
        s.p.signal(Signal::SIGINT);
        st = s.p.wait_exit(Duration::from_secs(10)).await;
    }
    if st.is_none() {
        s.p.signal_group(Signal::SIGKILL);
        st = s.p.wait_exit(Duration::from_secs(5)).await;
    }
    let _ = ctx;
    json!({"exit": st.map(|x| crate::process::exit_json(&x))})
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let ctx = Ctx::new(
        cfg,
        "service_failure",
        deny_all("service_failure: no tool calls expected"),
    )
    .await?;

    ctx.step("case 1: nonexistent model name (API rejects the request, nothing billed)");
    let mut args = ctx.base_args();
    if let Some(i) = args.iter().position(|a| a == "--model") {
        args[i + 1] = "claude-canterlot-nonexistent-model".to_owned();
    }
    let mut s1 = ctx.spawn_args(args, &[])?;
    let o1 = attempt(&mut s1, Duration::from_secs(90)).await?;
    let n1 = summarize(&ctx, &s1, &o1);
    let e1 = finish_proc(&ctx, &mut s1).await;
    let c1 = ctx.check(
        "invalid_model",
        o1.result
            .as_ref()
            .is_some_and(|(_, r)| r["is_error"] == true),
        json!({"observed": n1, "process": e1}),
    );

    ctx.step("case 2: unreachable HTTPS proxy, CLAUDE_CODE_MAX_RETRIES=1 (documented), wait for the terminal result");
    let mut s2 = ctx.spawn(
        &[],
        &[
            ("HTTPS_PROXY", DEAD_PROXY),
            ("HTTP_PROXY", DEAD_PROXY),
            ("CLAUDE_CODE_MAX_RETRIES", "1"),
        ],
    )?;
    let o2 = attempt(&mut s2, Duration::from_secs(120)).await?;
    let n2 = summarize(&ctx, &s2, &o2);
    let e2 = finish_proc(&ctx, &mut s2).await;
    let c2 = ctx.check(
        "unreachable_proxy_limited_retries",
        o2.result
            .as_ref()
            .is_some_and(|(_, r)| r["is_error"] == true),
        json!({"observed": n2, "process": e2}),
    );

    ctx.step("case 3: unreachable HTTPS proxy with default retries: record the retry schedule for 75s, then stop");
    let mut s3 = ctx.spawn(
        &[],
        &[("HTTPS_PROXY", DEAD_PROXY), ("HTTP_PROXY", DEAD_PROXY)],
    )?;
    let o3 = attempt(&mut s3, Duration::from_secs(75)).await?;
    let n3 = summarize(&ctx, &s3, &o3);
    let e3 = finish_proc(&ctx, &mut s3).await;
    let c3 = ctx.check(
        "unreachable_proxy_default_retries",
        s3.system.iter().any(|(_, v)| v["subtype"] == "api_retry"),
        json!({"observed": n3, "process": e3}),
    );

    let ok = c1 && c2 && c3;
    ctx.finish(
        if ok { "PASS" } else { "PARTIAL" },
        if ok {
            "API failures surface as system/api_retry events and an is_error result; model errors fail fast; see timings"
        } else {
            "some failure mode was not surfaced as expected; see check notes"
        },
    );
    Ok(())
}
