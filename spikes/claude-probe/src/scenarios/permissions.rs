//! Разрешения через `--permission-prompt-tool` и наш MCP-сервер на Rust:
//! allow для `touch probe.txt`, deny с сообщением для второго действия.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Ctx, TURN_TIMEOUT, result_summary};
use crate::broker::{self, PolicyAction};
use crate::config::Config;

pub const DENY_MESSAGE: &str =
    "Denied by the canterlot probe supervisor: only `touch probe.txt` is allowed in this scenario.";

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(cfg, "permissions", broker::deny_all("placeholder")).await?;
    let cwd = ctx.cwd.clone();
    let rec = ctx.rec.clone();
    ctx.broker.set_policy(Arc::new(move |args: &Value| {
        let tool = broker::requested_tool(args);
        let input = broker::requested_input(args);
        let probe_exists = cwd.join("probe.txt").exists();
        rec.note(
            json!({"policy_view": {"tool": tool, "command": input["command"],
                                        "tool_use_id": broker::requested_tool_use_id(args),
                                        "probe_txt_exists_before_decision": probe_exists}}),
        );
        if tool == "Bash" && input["command"].as_str().map(str::trim) == Some("touch probe.txt") {
            PolicyAction::Respond(broker::allow(&input))
        } else {
            PolicyAction::Respond(broker::deny(DENY_MESSAGE))
        }
    }));
    let mut s = ctx.spawn(&[], &[])?;

    ctx.step("turn: two Bash actions, first must be allowed, second denied");
    ctx.turns += 1;
    let out = s
        .turn(
            "Use the Bash tool for two separate commands, one tool call each, in this order: first `touch probe.txt`, then `touch second.txt`. \
             If a command is denied, do not retry it and do not try any alternative. Finish with one short sentence saying which commands ran.",
            TURN_TIMEOUT,
        )
        .await?;
    s.pump_for(Duration::from_secs(1)).await;
    let r = out.result.clone().unwrap_or_default();
    let seen: Vec<_> = std::iter::from_fn(|| ctx.perms.try_recv().ok()).collect();

    let probe_exists = ctx.cwd.join("probe.txt").exists();
    let second_exists = ctx.cwd.join("second.txt").exists();
    let uses = s.tools.uses().to_vec();

    let mut bound_all = !seen.is_empty();
    let mut allowed_ok = false;
    let mut denied_ok = false;
    let mut per_request = Vec::new();
    for p in &seen {
        let tool_use_id = broker::requested_tool_use_id(&p.arguments).map(str::to_owned);
        let input = broker::requested_input(&p.arguments);
        let matching_use = uses.iter().find(|u| Some(&u.id) == tool_use_id.as_ref());
        let bound = matching_use.is_some_and(|u| {
            u.input == input && Some(u.name.as_str()) == Some(broker::requested_tool(&p.arguments))
        });
        bound_all &= bound;
        let behavior = p
            .decision
            .as_ref()
            .map(|d| d["behavior"].clone())
            .unwrap_or(Value::Null);
        let tool_result = tool_use_id
            .as_deref()
            .and_then(|id| s.tools.result_for(id))
            .cloned();
        let result_t = tool_use_id.as_deref().and_then(|id| {
            s.users
                .iter()
                .find(|(_, v)| {
                    crate::protocol::tool_results(v)
                        .iter()
                        .any(|tr| tr.tool_use_id == id)
                })
                .map(|(t, _)| *t)
        });
        if behavior == "allow" {
            allowed_ok = tool_result.as_ref().is_some_and(|tr| !tr.is_error)
                && result_t.is_some_and(|t| t >= p.t_ms)
                && probe_exists;
        }
        if behavior == "deny" {
            denied_ok = tool_result
                .as_ref()
                .is_some_and(|tr| tr.is_error && tr.text.contains("canterlot probe supervisor"))
                && !second_exists;
        }
        per_request.push(json!({
            "call_id": p.call_id,
            "request_arguments_keys": p.arguments.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
            "tool_use_id": tool_use_id,
            "input": input,
            "bound_to_assistant_tool_use": bound,
            "decision": p.decision,
            "request_t_ms": p.t_ms,
            "tool_result_t_ms": result_t,
            "tool_result": tool_result.map(|tr| json!({"is_error": tr.is_error, "text": tr.text})),
        }));
    }
    let bind_ok = ctx.check(
        "request_binds_to_single_tool_use",
        bound_all,
        json!({"requests": per_request}),
    );
    let allow_ok = ctx.check(
        "tool_runs_only_after_allow",
        allowed_ok,
        json!({"probe_txt_exists_after_turn": probe_exists, "see": "policy_view notes: probe_txt_exists_before_decision"}),
    );
    let deny_ok = ctx.check(
        "deny_surfaced_to_model",
        denied_ok,
        json!({"second_txt_exists": second_exists, "final_text": r["result"],
               "permission_denials_in_result": r["permission_denials"]}),
    );
    ctx.check(
        "turn_result",
        r["subtype"] == "success",
        json!({"result": result_summary(&r)}),
    );
    ctx.rec.note(json!({"system_messages_seen": s.system.iter().map(|(_, v)| v["subtype"].clone()).collect::<Vec<_>>()}));

    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.rec
        .note(json!({"exit_after_eof": status.map(|st| crate::process::exit_json(&st))}));

    let ok = bind_ok && allow_ok && deny_ok;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "permission requests reached our Rust MCP tool with tool name, exact input and tool_use_id; tool ran only after allow; deny message reached the model"
        } else {
            "see check notes"
        },
    );
    Ok(())
}
