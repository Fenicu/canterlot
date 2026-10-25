//! Сессия (запуск + изоляция) и ведение одного хода с реакцией на события.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::appserver::{AppServer, Event, SpawnOptions};
use crate::evidence::Recorder;
use crate::isolation;
use crate::protocol::{
    self, ErrorNotification, ItemNotification, RequestId, RpcError, ThreadItem, ThreadStartResult,
    TurnError, TurnNotification, TurnStartResult, TurnStatus,
};

pub const RPC_WAIT: Duration = Duration::from_secs(30);
/// Шаг опроса ответов на действия; точные времена берутся из t_ms в evidence.
const POLL: Duration = Duration::from_millis(20);

#[derive(Default)]
pub struct SessionOptions {
    pub experimental: bool,
    pub extra_args: Vec<String>,
    pub extra_env: Vec<(String, String)>,
    /// Пропустить проверку изоляции (только для сценария, где она заведомо меняется).
    pub skip_isolation_check: bool,
}

pub struct Session {
    pub srv: AppServer,
    pub scratch: PathBuf,
}

impl Session {
    /// Запуск app-server, рукопожатие и обязательная проверка изоляции до любого хода модели.
    pub async fn open(
        rec: &Recorder,
        scratch: &Path,
        opts: SessionOptions,
    ) -> anyhow::Result<Self> {
        let srv = AppServer::spawn(
            SpawnOptions {
                scratch,
                extra_args: opts.extra_args,
                extra_env: opts.extra_env,
            },
            rec.clone(),
        )?;
        let init = srv.initialize(opts.experimental).await?;
        rec.note(
            json!({ "initialized": { "experimentalApi": opts.experimental,
            "platformOs": init["platformOs"], "userAgent": init["userAgent"] } }),
        );
        let read = srv
            .call_quiet(
                "config/read",
                json!({ "includeLayers": true, "cwd": scratch.to_string_lossy() }),
                RPC_WAIT,
            )
            .await?
            .map_err(|e| anyhow!("config/read failed: {}", e.message))?;
        if opts.skip_isolation_check {
            rec.note(json!({ "isolation": "check skipped by scenario" }));
        } else {
            let summary = isolation::check_effective_config(&read, scratch)?;
            rec.note(json!({ "isolation": summary }));
        }
        Ok(Self {
            srv,
            scratch: scratch.to_path_buf(),
        })
    }

    pub async fn start_thread(&self, ephemeral: bool) -> anyhow::Result<ThreadStartResult> {
        let v = self
            .srv
            .call_ok(
                "thread/start",
                isolation::thread_start_params(&self.scratch, ephemeral),
                RPC_WAIT,
            )
            .await?;
        let res: ThreadStartResult = serde_json::from_value(v).context("thread/start result")?;
        self.srv.recorder().note(json!({ "thread_started": {
            "threadId": res.thread.id,
            "model": res.model, "reasoningEffort": res.reasoning_effort,
            "approvalPolicy": res.approval_policy, "sandbox": res.sandbox,
            "instructionSources": res.instruction_sources,
        }}));
        Ok(res)
    }
}

/// Действие, которое сценарий запрашивает в ответ на событие.
#[derive(Debug, Clone)]
pub enum Action {
    Respond {
        id: RequestId,
        result: Value,
    },
    /// Сознательно не отвечать на запрос сервера (например, перед прерыванием).
    Defer {
        id: RequestId,
    },
    Interrupt,
    Steer {
        text: String,
        client_id: String,
    },
    StartTurn {
        text: String,
        client_id: String,
    },
    /// Прекратить ожидание хода (например, перед SIGKILL).
    Stop,
}

pub struct TurnState {
    pub thread_id: String,
    pub main_turn_id: Option<String>,
    pub started: Instant,
    pub items_completed: Vec<ThreadItem>,
    pub items_started: Vec<ThreadItem>,
}

#[derive(Debug, Default)]
pub struct TurnOutcome {
    pub turn_id: String,
    pub status: Option<TurnStatus>,
    pub error: Option<TurnError>,
    pub agent_text: String,
    pub delta_text: String,
    pub delta_count: usize,
    pub items_completed: Vec<ThreadItem>,
    pub errors: Vec<ErrorNotification>,
    pub server_requests: Vec<(RequestId, String, Value)>,
    pub other_turns: Vec<(String, Option<TurnStatus>)>,
    pub action_results: Vec<(String, Result<Value, RpcError>)>,
    pub t_response_ms: Option<u128>,
    pub t_started_ms: Option<u128>,
    pub t_first_delta_ms: Option<u128>,
    pub t_completed_ms: Option<u128>,
    pub timed_out: bool,
    pub stopped: bool,
    pub stdout_closed: bool,
    pub start_error: Option<RpcError>,
}

pub type Hook<'a> = dyn FnMut(&Event, &TurnState) -> Vec<Action> + 'a;

pub fn text_input(text: &str) -> Value {
    json!([{ "type": "text", "text": text, "text_elements": [] }])
}

/// Безопасный ответ по умолчанию на запрос сервера, который сценарий не обработал.
/// Отказ выбирается только из `availableDecisions`, если сервер их прислал.
fn default_response(method: &str, params: &Value) -> Option<Value> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let offered = params["availableDecisions"].as_array();
            let decision = match offered {
                Some(list) if !list.iter().any(|d| d == "decline") => "cancel",
                _ => "decline",
            };
            Some(protocol::approval_decision(decision))
        }
        "mcpServer/elicitation/request" => Some(json!({ "action": "decline", "content": null })),
        "item/permissions/requestApproval" => Some(json!({ "permissions": {} })),
        _ => None,
    }
}

/// Запускает ход и ведёт его до `turn/completed` всех отслеживаемых ходов.
pub async fn drive_turn(
    srv: &mut AppServer,
    thread_id: &str,
    mut params: Value,
    hook: &mut Hook<'_>,
    deadline: Duration,
) -> anyhow::Result<TurnOutcome> {
    let rec = srv.recorder().clone();
    params["threadId"] = json!(thread_id);
    let t0 = Instant::now();
    let (_, mut start_rx) = srv.send_request("turn/start", params, false).await?;
    let mut start_pending = true;
    let mut out = TurnOutcome::default();
    let mut state = TurnState {
        thread_id: thread_id.to_string(),
        main_turn_id: None,
        started: t0,
        items_completed: Vec::new(),
        items_started: Vec::new(),
    };
    let mut tracked: HashSet<String> = HashSet::new();
    let mut completed: HashSet<String> = HashSet::new();
    let mut pending_actions: Vec<(String, oneshot::Receiver<Result<Value, RpcError>>)> = Vec::new();

    loop {
        if start_pending {
            match start_rx.try_recv() {
                Ok(Ok(v)) => {
                    start_pending = false;
                    out.t_response_ms = Some(t0.elapsed().as_millis());
                    let r: TurnStartResult =
                        serde_json::from_value(v).context("turn/start result")?;
                    rec.note(json!({ "turn_start_response": { "turnId": r.turn.id,
                        "status": format!("{:?}", r.turn.status), "after_ms": out.t_response_ms } }));
                    state.main_turn_id.get_or_insert(r.turn.id.clone());
                    tracked.insert(r.turn.id);
                }
                Ok(Err(e)) => {
                    rec.note(
                        json!({ "turn_start_rejected": { "code": e.code, "message": e.message } }),
                    );
                    out.start_error = Some(e);
                    return Ok(out);
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => bail!("turn/start: server closed"),
            }
        }
        pending_actions.retain_mut(|(label, rx)| match rx.try_recv() {
            Ok(r) => {
                rec.note(json!({ "action_result": { "action": label,
                    "after_ms": t0.elapsed().as_millis() as u64,
                    "result": match &r { Ok(v) => v.clone(), Err(e) => json!({"error": e}) } } }));
                if let (true, Ok(v)) = (label.starts_with("turn/start"), &r)
                    && let Some(id) = v["turn"]["id"].as_str()
                {
                    tracked.insert(id.to_string());
                }
                out.action_results.push((label.clone(), r));
                false
            }
            Err(oneshot::error::TryRecvError::Empty) => true,
            Err(oneshot::error::TryRecvError::Closed) => false,
        });

        if !tracked.is_empty() && tracked.is_subset(&completed) && pending_actions.is_empty() {
            break;
        }
        let remaining = deadline.saturating_sub(t0.elapsed());
        if remaining.is_zero() {
            out.timed_out = true;
            rec.note(json!({ "turn_timeout_ms": deadline.as_millis() as u64 }));
            break;
        }
        let Some(ev) = srv.next_event(remaining.min(POLL)).await else {
            continue;
        };
        if let Event::StdoutClosed = ev {
            out.stdout_closed = true;
            break;
        }
        // учёт стандартных событий хода
        if let Event::Notification { method, params } = &ev {
            match method.as_str() {
                "turn/started" => {
                    if let Ok(n) = serde_json::from_value::<TurnNotification>(params.clone())
                        && n.thread_id == thread_id
                    {
                        out.t_started_ms.get_or_insert(t0.elapsed().as_millis());
                        state.main_turn_id.get_or_insert(n.turn.id.clone());
                        tracked.insert(n.turn.id);
                    }
                }
                "turn/completed" => {
                    if let Ok(n) = serde_json::from_value::<TurnNotification>(params.clone())
                        && n.thread_id == thread_id
                    {
                        if state.main_turn_id.as_deref() == Some(n.turn.id.as_str()) {
                            out.t_completed_ms = Some(t0.elapsed().as_millis());
                            out.status = Some(n.turn.status.clone());
                            out.error = n.turn.error.clone();
                        } else {
                            out.other_turns
                                .push((n.turn.id.clone(), Some(n.turn.status.clone())));
                        }
                        tracked.insert(n.turn.id.clone());
                        completed.insert(n.turn.id);
                    }
                }
                // поздние события прошлых ходов (например, завершение команды после interrupt) не смешиваем
                "item/started" => {
                    if let Ok(n) = serde_json::from_value::<ItemNotification>(params.clone())
                        && (tracked.is_empty() || tracked.contains(&n.turn_id))
                    {
                        state.items_started.push(n.item);
                    }
                }
                "item/completed" => {
                    if let Ok(n) = serde_json::from_value::<ItemNotification>(params.clone())
                        && (tracked.is_empty() || tracked.contains(&n.turn_id))
                    {
                        if let ThreadItem::AgentMessage { text, .. } = &n.item {
                            if !out.agent_text.is_empty() {
                                out.agent_text.push('\n');
                            }
                            out.agent_text.push_str(text);
                        }
                        state.items_completed.push(n.item);
                    }
                }
                "item/agentMessage/delta" => {
                    if let Some(d) = params["delta"].as_str() {
                        out.t_first_delta_ms.get_or_insert(t0.elapsed().as_millis());
                        out.delta_text.push_str(d);
                        out.delta_count += 1;
                    }
                }
                "error" => {
                    if let Ok(n) = serde_json::from_value::<ErrorNotification>(params.clone()) {
                        out.errors.push(n);
                    }
                }
                _ => {}
            }
        }

        let actions = hook(&ev, &state);
        let mut answered: HashSet<RequestId> = HashSet::new();
        for a in actions {
            match a {
                Action::Respond { id, result } => {
                    srv.respond(&id, result).await?;
                    answered.insert(id);
                }
                Action::Defer { id } => {
                    rec.note(json!({ "deferred_server_request": id.to_value() }));
                    answered.insert(id);
                }
                Action::Interrupt => {
                    let turn_id = state
                        .main_turn_id
                        .clone()
                        .ok_or_else(|| anyhow!("interrupt before turn id known"))?;
                    rec.note(
                        json!({ "interrupt_requested_at_ms": t0.elapsed().as_millis() as u64 }),
                    );
                    let (_, rx) = srv
                        .send_request(
                            "turn/interrupt",
                            json!({ "threadId": thread_id, "turnId": turn_id }),
                            false,
                        )
                        .await?;
                    pending_actions.push(("turn/interrupt".into(), rx));
                }
                Action::Steer { text, client_id } => {
                    let turn_id = state
                        .main_turn_id
                        .clone()
                        .ok_or_else(|| anyhow!("steer before turn id known"))?;
                    let (_, rx) = srv
                        .send_request(
                            "turn/steer",
                            json!({ "threadId": thread_id, "expectedTurnId": turn_id,
                                "input": text_input(&text), "clientUserMessageId": client_id }),
                            false,
                        )
                        .await?;
                    pending_actions.push(("turn/steer".into(), rx));
                }
                Action::StartTurn { text, client_id } => {
                    let (_, rx) = srv
                        .send_request(
                            "turn/start",
                            json!({ "threadId": thread_id, "input": text_input(&text),
                                "clientUserMessageId": client_id }),
                            false,
                        )
                        .await?;
                    pending_actions.push((format!("turn/start concurrent ({client_id})"), rx));
                }
                Action::Stop => {
                    out.stopped = true;
                }
            }
        }
        if let Event::ServerRequest { id, method, params } = &ev {
            out.server_requests
                .push((id.clone(), method.clone(), params.clone()));
            if !answered.contains(id) {
                match default_response(method, params) {
                    Some(result) => {
                        rec.note(json!({ "auto_declined_unexpected_request": { "id": id.to_value(), "method": method } }));
                        srv.respond(id, result).await?;
                    }
                    None => {
                        rec.note(json!({ "unanswered_server_request": { "id": id.to_value(), "method": method } }));
                    }
                }
            }
        }
        if out.stopped {
            break;
        }
    }
    out.turn_id = state.main_turn_id.clone().unwrap_or_default();
    out.items_completed = state.items_completed;
    rec.note(json!({ "turn_outcome": {
        "turnId": out.turn_id,
        "status": out.status.as_ref().map(|s| format!("{s:?}")),
        "error": out.error.as_ref().map(|e| json!({"message": e.message, "codexErrorInfo": e.codex_error_info})),
        "agent_text": out.agent_text,
        "delta_count": out.delta_count,
        "deltas_equal_final_text": !out.delta_text.is_empty() && out.agent_text.contains(out.delta_text.trim()),
        "items_completed": out.items_completed.iter().map(|i| i.kind()).collect::<Vec<_>>(),
        "server_requests": out.server_requests.iter().map(|(id, m, _)| json!({"id": id.to_value(), "method": m})).collect::<Vec<_>>(),
        "other_turns": out.other_turns.iter().map(|(id, s)| json!({"turnId": id, "status": s.as_ref().map(|s| format!("{s:?}"))})).collect::<Vec<_>>(),
        "t_response_ms": out.t_response_ms, "t_started_ms": out.t_started_ms,
        "t_first_delta_ms": out.t_first_delta_ms, "t_completed_ms": out.t_completed_ms,
        "timed_out": out.timed_out, "stopped": out.stopped, "stdout_closed": out.stdout_closed,
    }}));
    Ok(out)
}

/// Ход без особой логики: всё неожиданное отклоняется по умолчанию.
pub async fn simple_turn(
    srv: &mut AppServer,
    thread_id: &str,
    text: &str,
    client_id: Option<&str>,
    deadline: Duration,
) -> anyhow::Result<TurnOutcome> {
    let mut params = json!({ "input": text_input(text) });
    if let Some(c) = client_id {
        params["clientUserMessageId"] = json!(c);
    }
    let mut hook = |_: &Event, _: &TurnState| Vec::new();
    drive_turn(srv, thread_id, params, &mut hook, deadline).await
}

/// Нормализует команду из запроса одобрения: снимает обёртку `bash -lc '...'`.
pub fn normalize_command(cmd: &str) -> String {
    let mut s = cmd.trim();
    for prefix in [
        "/bin/bash -lc ",
        "/usr/bin/bash -lc ",
        "bash -lc ",
        "/bin/bash -c ",
        "bash -c ",
        "/bin/sh -c ",
        "sh -c ",
        "/bin/zsh -lc ",
        "zsh -lc ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'\'' || bytes[0] == b'"')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        s = &s[1..s.len() - 1];
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_shell_wrappers() {
        assert_eq!(
            normalize_command("/bin/bash -lc 'touch probe.txt'"),
            "touch probe.txt"
        );
        assert_eq!(
            normalize_command("bash -lc \"touch probe.txt\""),
            "touch probe.txt"
        );
        assert_eq!(normalize_command("touch probe.txt"), "touch probe.txt");
        assert_ne!(
            normalize_command("/bin/bash -lc 'touch probe.txt; rm -rf x'"),
            "touch probe.txt"
        );
    }

    #[test]
    fn default_response_declines_approvals_only() {
        assert_eq!(
            default_response("item/commandExecution/requestApproval", &json!({})),
            Some(json!({"decision": "decline"}))
        );
        assert_eq!(
            default_response(
                "item/commandExecution/requestApproval",
                &json!({"availableDecisions": ["accept", "cancel"]})
            ),
            Some(json!({"decision": "cancel"}))
        );
        assert!(default_response("item/tool/requestUserInput", &json!({})).is_none());
    }
}
