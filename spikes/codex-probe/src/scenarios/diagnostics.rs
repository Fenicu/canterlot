//! Диагностика без платного запроса: версия, рукопожатие, аккаунт, модели, изоляция.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Verdict, run_cli};
use crate::driver::{RPC_WAIT, Session, SessionOptions};
use crate::evidence::Recorder;
use crate::isolation;

pub async fn run(rec: &Recorder, scratch: &Path) -> anyhow::Result<Verdict> {
    rec.step("codex --version");
    let (code, version, _) = run_cli(&["--version"]).await?;
    rec.note(json!({ "cli_version": version.trim(), "exit": code }));

    rec.step("codex app-server --help (stability label)");
    let (_, help, _) = run_cli(&["app-server", "--help"]).await?;
    rec.note(json!({ "app_server_help_first_line": help.lines().next().unwrap_or_default() }));

    rec.step("execpolicy: user rules + project guard (decision only)");
    let home = std::env::var("HOME").unwrap_or_default();
    let user_rules = format!("{home}/.codex/rules/default.rules");
    let guard = scratch.join(".codex/rules/canterlot-guard.rules");
    let guard_s = guard.to_string_lossy().to_string();
    let mut decisions = serde_json::Map::new();
    for cmd in [
        "curl -sS https://example.com",
        "ssh host",
        "git push",
        "sudo apt-get install x",
        "touch probe.txt",
        "sleep 20",
    ] {
        let mut args = vec![
            "execpolicy",
            "check",
            "--rules",
            &user_rules,
            "--rules",
            &guard_s,
            "--",
        ];
        args.extend(cmd.split_whitespace());
        let (_, out, err) = run_cli(&args).await?;
        let decision = serde_json::from_str::<Value>(&out)
            .map(|v| v["decision"].clone())
            .unwrap_or_else(|_| json!(format!("unparsed: {}", err.trim())));
        decisions.insert(cmd.to_string(), decision);
    }
    rec.note(json!({ "execpolicy_effective_decisions": decisions }));

    rec.step("spawn app-server, initialize, verify isolation");
    let mut s = Session::open(rec, scratch, SessionOptions::default()).await?;

    rec.step("account/read without token refresh (no paid request)");
    let account = s
        .srv
        .call_ok("account/read", json!({ "refreshToken": false }), RPC_WAIT)
        .await?;
    let account_type = account["account"]["type"]
        .as_str()
        .unwrap_or("none")
        .to_string();
    rec.note(json!({ "auth_ready": account["account"].is_object(),
        "account_type": account_type, "plan": account["account"]["planType"],
        "requiresOpenaiAuth": account["requiresOpenaiAuth"] }));

    rec.step("model/list");
    let models = s
        .srv
        .call_quiet(
            "model/list",
            json!({ "limit": 100, "includeHidden": false }),
            RPC_WAIT,
        )
        .await?
        .map_err(|e| anyhow::anyhow!("model/list: {}", e.message))?;
    let list: Vec<Value> = models["data"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            json!({
                "id": m["id"], "isDefault": m["isDefault"], "description": m["description"],
                "defaultReasoningEffort": m["defaultReasoningEffort"],
                "efforts": m["supportedReasoningEfforts"].as_array().map(|a| a.iter().map(|e| e["reasoningEffort"].clone()).collect::<Vec<_>>()),
                "inputModalities": m["inputModalities"],
            })
        })
        .collect();
    let chosen = list.iter().find(|m| m["id"] == isolation::MODEL).cloned();
    let effort_ok = chosen.as_ref().is_some_and(|m| {
        m["efforts"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e == isolation::EFFORT))
    });
    rec.note(json!({ "models": list, "chosen_model": isolation::MODEL, "chosen_effort": isolation::EFFORT,
        "reason": "cheapest listed model ('Fast and affordable model for easier tasks') at its lowest effort",
        "chosen_available": chosen.is_some(), "effort_supported": effort_ok }));

    rec.step("mcpServerStatus/list after isolation check (servers must not be running)");
    let mcp = s
        .srv
        .call_ok("mcpServerStatus/list", json!({}), RPC_WAIT)
        .await?;
    let mcp_summary: Vec<Value> = mcp["data"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            json!({ "name": m["name"], "runtimeStatus": m["runtimeStatus"],
            "serverInfo": m["serverInfo"], "toolCount": m["tools"].as_object().map(|t| t.len()) })
        })
        .collect();
    let mcp_idle = mcp_summary
        .iter()
        .all(|m| m["runtimeStatus"].is_null() && m["serverInfo"].is_null());
    rec.note(json!({ "mcp_status": mcp_summary, "all_mcp_not_started": mcp_idle }));

    rec.step("thread/start (ephemeral, no model request) to read per-thread effective settings");
    let t = s.start_thread(true).await?;

    rec.step("drain notifications briefly");
    let until = Instant::now() + Duration::from_secs(2);
    let mut startup_notifications = Vec::new();
    while let Some(remaining) = until.checked_duration_since(Instant::now()) {
        if let Some(crate::appserver::Event::Notification { method, .. }) =
            s.srv.next_event(remaining).await
        {
            startup_notifications.push(method);
        } else {
            break;
        }
    }
    rec.note(json!({ "notifications_seen": startup_notifications }));
    let mcp_started = startup_notifications
        .iter()
        .any(|m| m == "mcpServer/startupStatus/updated");

    rec.step("shutdown via stdin EOF");
    let t0 = Instant::now();
    s.srv.close_stdin().await;
    let exited = s.srv.wait_exit(Duration::from_secs(10)).await;
    rec.note(
        json!({ "eof_exit_after_ms": t0.elapsed().as_millis() as u64, "exited": exited.is_some() }),
    );
    if exited.is_none() {
        s.srv.shutdown().await?;
    }

    let ok = version.contains("0.164.0")
        && account_type != "none"
        && chosen.is_some()
        && effort_ok
        && mcp_idle
        && !mcp_started
        && t.model.as_deref() == Some(isolation::MODEL);
    Ok(if ok {
        Verdict::new(
            "PASS",
            "version, initialize, account/read (ChatGPT auth ready), model/list and isolation verified without a paid request",
        )
    } else {
        Verdict::new("FAIL", "one of the diagnostics checks failed; see notes")
    })
}
