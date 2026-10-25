//! Диагностика без платного запроса: путь, версия, готовность входа,
//! «сухой» запуск stream-json процесса без единого ввода.

use std::time::Duration;

use serde_json::{Value, json};

use super::Ctx;
use crate::broker::deny_all;
use crate::config::Config;
use crate::protocol::{self, Kind};

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(
        cfg,
        "diagnostics",
        deny_all("diagnostics: no tool calls expected"),
    )
    .await?;

    ctx.step("locate CLI");
    let resolved = std::fs::canonicalize(&cfg.claude).ok();
    let located = ctx.check(
        "cli_located",
        resolved.is_some(),
        json!({"configured": cfg.claude, "resolved": resolved}),
    );

    ctx.step("version");
    let version = ctx.exec(&["--version"], &[]).await?;
    let version_line = version.stdout.trim().to_owned();
    let version_ok = ctx.check(
        "version",
        version.code == Some(0) && version_line.contains("Claude Code"),
        json!({"stdout": version_line, "ms": version.ms}),
    );

    ctx.step("auth status (documented `claude auth status --json`, no model request)");
    let auth = ctx.exec(&["auth", "status", "--json"], &[]).await?;
    let auth_json: Value = serde_json::from_str(&auth.stdout).unwrap_or(Value::Null);
    let logged_in = auth_json.get("loggedIn").and_then(Value::as_bool);
    let keys: Vec<String> = auth_json
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let auth_ok = ctx.check(
        "auth_ready",
        auth.code == Some(0) && logged_in == Some(true),
        json!({"exit_code": auth.code, "loggedIn": logged_in, "fields": keys,
               "authMethod": auth_json.get("authMethod"), "apiProvider": auth_json.get("apiProvider"),
               "subscriptionType": auth_json.get("subscriptionType"), "ms": auth.ms}),
    );

    ctx.step("dry start: stream-json process with the full base flag set, no input written");
    let mut s = ctx.spawn(&[], &[])?;
    let mut first_frames: Vec<Value> = Vec::new();
    s.pump_until(Duration::from_secs(20), |_, v| {
        first_frames.push(json!({"type": v["type"], "subtype": v["subtype"]}));
        false
    })
    .await;
    let alive_without_input = s.p.try_exit().is_none();
    let init_before_input = s.init.is_some();
    ctx.check(
        "dry_start_frames_before_any_input",
        true,
        json!({"frames": first_frames, "init_emitted_before_input": init_before_input,
               "alive_after_20s_without_input": alive_without_input}),
    );
    let tree = s.p.record_tree("dry start, no input");
    let mcp_running = tree.iter().any(|p| p.cmdline.contains("mcp-permission"));
    ctx.check(
        "permission_mcp_server_spawned_at_startup",
        mcp_running,
        json!({"processes": tree.len()}),
    );

    ctx.step("close stdin without any input: observe exit");
    let t_close = ctx.rec.elapsed_ms();
    s.p.close_stdin();
    s.pump_for(Duration::from_secs(15)).await;
    let status = s.p.wait_exit(Duration::from_secs(15)).await;
    let t_exit = ctx.rec.elapsed_ms();
    let results_on_eof: Vec<Value> = s
        .results
        .iter()
        .map(|(_, r)| super::result_summary(r))
        .collect();
    ctx.check(
        "eof_without_input_exits",
        status.is_some(),
        json!({"exit": status.map(|st| crate::process::exit_json(&st)), "ms_after_eof": t_exit - t_close,
               "results_emitted": results_on_eof,
               "other_frames": s.system.iter().map(|(_, v)| protocol::classify(v)).filter(|k| *k != Kind::SystemInit).map(|k| format!("{k:?}")).collect::<Vec<_>>()}),
    );
    if status.is_none() {
        s.p.signal_group(nix::sys::signal::Signal::SIGKILL);
    }

    let (verdict, reason) = if located && version_ok && auth_ok {
        (
            "PASS",
            format!("CLI {version_line} found, logged in, startup works without a paid request"),
        )
    } else {
        (
            "FAIL",
            "CLI not found, version unreadable or not logged in".to_owned(),
        )
    };
    ctx.turns = 0;
    ctx.finish(verdict, &reason);
    Ok(())
}
