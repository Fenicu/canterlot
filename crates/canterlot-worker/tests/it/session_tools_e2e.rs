#![forbid(unsafe_code)]

//! Межсессионные инструменты целиком: настоящий хаб, воркер, сопровождение и синтетические CLI.
//! Оркестратор и потомок управляются скриптами, каждый сценарий сверяет поток WSS с журналом.

use crate::fake_exec;
#[path = "support/pilot.rs"]
mod pilot;
#[path = "support/process_guard.rs"]
mod process_guard;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use canterlot_hub::{config::ActivityConfig, testkit::Method};
use canterlot_worker::config::ClaudeCodeConfig;
use pilot::*;
use rustix::process::Signal;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::sleep;
use uuid::Uuid;

const QUIET: Duration = Duration::from_millis(300);
const HARNESSES: [&str; 3] = [
    "synthetic",
    "synthetic-exit-on-interrupt",
    "synthetic-codex",
];

fn options() -> PilotOptions {
    PilotOptions {
        activity: ActivityConfig {
            debounce: ms(50),
            tick: ms(100),
            idle_quiet: QUIET,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn git(dir: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=e2e",
            "-c",
            "user.email=e2e@canterlot.invalid",
        ])
        .args(arguments)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Сценарий следующего хода: короткий текст со ссылкой на файл шагов, чтобы цепочку из
/// многих ходов не приходилось вкладывать одну в другую.
fn hop(path: &Path) -> String {
    script(json!([{"script_file": path}]))
}

fn send_step(target: Uuid, text: &str, subscribe: bool) -> Value {
    json!({"agent_call": {"tool": "sessions.send", "arguments": {
        "session_id": target, "text": text, "subscribe": subscribe,
    }}})
}

fn background(state: &str) -> Value {
    json!({"background": {"tasks": [
        {"task_id": "b1", "kind": "bash", "state": state, "description": "сборка"}
    ]}})
}

struct Bench {
    p: Pilot,
    ws: Recorder,
}

impl Bench {
    async fn new(pool: &PgPool) -> Self {
        Self::with(pool, options()).await
    }

    async fn with(pool: &PgPool, options: PilotOptions) -> Self {
        let p = Pilot::new(pool, options).await;
        let mut ws = p.ws().await;
        ws.subscribe("list", "conversations").await;
        let bench = Self { p, ws };
        let entries: Vec<Value> = HARNESSES
            .iter()
            .map(|harness| json!({"harness": harness, "model": "synthetic-small"}))
            .collect();
        bench.whitelist(json!(entries)).await;
        bench
    }

    async fn whitelist(&self, entries: Value) {
        let response = self
            .p
            .call(
                Method::PUT,
                "/api/v1/spawn-whitelist",
                Some(json!({"entries": entries})),
            )
            .await;
        assert_eq!(response.status, 200);
    }

    async fn watch(&mut self, id: Uuid) {
        self.ws
            .subscribe(&id.to_string(), &format!("conversation:{id}"))
            .await;
    }

    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.p.work.join(name);
        fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("README"), "начало\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "Начало"]);
        dir
    }

    async fn session(&mut self, harness: &str, workdir: &Path) -> Uuid {
        self.session_on(harness, "synthetic-large", "high", workdir)
            .await
    }

    async fn session_on(
        &mut self,
        harness: &str,
        model: &str,
        effort: &str,
        workdir: &Path,
    ) -> Uuid {
        let created = self
            .p
            .post(
                "/api/v1/conversations",
                Some(json!({
                    "machine_id": self.p.machine,
                    "harness": harness,
                    "workdir": workdir.to_str().unwrap(),
                    "model": model,
                    "effort": effort,
                    "permission_mode": "owner",
                    "start": true,
                })),
                201,
            )
            .await;
        let id = uuid(&created["conversation_id"]);
        self.running(id).await;
        self.watch(id).await;
        id
    }

    async fn running(&self, id: Uuid) {
        until("execution running", async || {
            self.p.detail(id).await["execution"]["state"] == "running"
        })
        .await;
    }

    async fn stopped(&self, id: Uuid) {
        until("execution stopped", async || {
            let state = &self.p.detail(id).await["execution"]["state"];
            state.is_null() || state == "stopped"
        })
        .await;
    }

    /// Ответы хаба на вызовы инструментов: блоки результата вне хода, по порядку.
    async fn results(&self, id: Uuid) -> Vec<String> {
        self.p
            .history(id)
            .await
            .iter()
            .filter(|item| {
                item["kind"] == "block"
                    && item["block_kind"] == "tool_result"
                    && item["state"] == "final"
                    && item["input_command_id"].is_null()
            })
            .map(|item| item["content"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Вызов инструмента ходом сессии. Ошибка инструмента приходит текстом `код: сообщение`.
    async fn tool(&self, id: Uuid, tool: &str, arguments: Value) -> Result<Value, String> {
        let seen = self.results(id).await.len();
        self.p
            .submit(
                id,
                &script(json!([{"agent_call": {"tool": tool, "arguments": arguments}}])),
                &[],
            )
            .await;
        let mut reply = String::new();
        until("tool result", async || {
            let results = self.results(id).await;
            match results.get(seen) {
                Some(found) => {
                    reply.clone_from(found);
                    true
                }
                None => false,
            }
        })
        .await;
        serde_json::from_str(&reply).map_err(|_| reply)
    }

    async fn spawn_child(&mut self, parent: Uuid, arguments: Value) -> (Uuid, Value) {
        let answer = self
            .tool(parent, "sessions.spawn", arguments)
            .await
            .expect("spawn answers");
        assert_eq!(answer["status"], "started", "{answer}");
        let child = uuid(&answer["session_id"]);
        self.watch(child).await;
        (child, answer)
    }

    /// Текст вводов от хаба: события подписки и служебные сообщения.
    async fn events(&self, id: Uuid) -> Vec<String> {
        sqlx::query_scalar(
            "select content->0->>'text' from inputs
             where conversation_id=$1 and origin->>'kind'='event' order by input_seq",
        )
        .bind(id)
        .fetch_all(&self.p.pool)
        .await
        .unwrap()
    }

    /// События о состоянии цели, без служебных сообщений о запуске и приёме.
    async fn reports(&self, id: Uuid) -> Vec<String> {
        let mut reports = self.events(id).await;
        reports.retain(|text| text.contains(" is now "));
        reports
    }

    async fn report_with(&self, id: Uuid, needle: &str) -> String {
        let mut found = String::new();
        until("report arrives", async || {
            match self
                .reports(id)
                .await
                .into_iter()
                .find(|text| text.contains(needle))
            {
                Some(text) => {
                    found = text;
                    true
                }
                None => false,
            }
        })
        .await;
        found
    }

    async fn event_with(&self, id: Uuid, needle: &str) -> String {
        let mut found = String::new();
        until("event arrives", async || {
            match self
                .events(id)
                .await
                .into_iter()
                .find(|text| text.contains(needle))
            {
                Some(text) => {
                    found = text;
                    true
                }
                None => false,
            }
        })
        .await;
        found
    }

    async fn input_of_event(&self, id: Uuid, needle: &str) -> (Uuid, String) {
        sqlx::query_as(
            "select id,state from inputs where conversation_id=$1 and origin->>'kind'='event'
             and content->0->>'text' like '%'||$2||'%' order by input_seq limit 1",
        )
        .bind(id)
        .bind(needle)
        .fetch_one(&self.p.pool)
        .await
        .unwrap()
    }

    /// Очередь разошлась: ни заданий, ни незавершённых вводов, сессия не в ходу.
    async fn quiet(&self, ids: &[Uuid]) {
        until("everything delivered and run", async || {
            let busy: bool = sqlx::query_scalar(
                "select exists(select 1 from scheduled_jobs where kind='session_task'
                                 and state in ('pending','running') and conversation_id=any($1))
                     or exists(select 1 from inputs where conversation_id=any($1)
                                 and state in ('queued','dispatched','on_machine','transmitting'))
                     or exists(select 1 from queue_state where conversation_id=any($1)
                                 and run_slot<>'free')",
            )
            .bind(ids)
            .fetch_one(&self.p.pool)
            .await
            .unwrap();
            !busy
        })
        .await;
    }

    async fn subscriptions(&self, id: Uuid) -> Vec<(Uuid, Uuid, Option<String>)> {
        sqlx::query_as(
            "select subscriber_id,target_id,closed_reason from session_subscriptions
             where subscriber_id=$1 or target_id=$1",
        )
        .bind(id)
        .fetch_all(&self.p.pool)
        .await
        .unwrap()
    }

    async fn resume(&self, id: Uuid) {
        self.p
            .post(
                &format!("/api/v1/conversations/{id}/resume"),
                Some(json!({"continue_queue": true})),
                201,
            )
            .await;
        self.running(id).await;
    }

    async fn stop(&self, id: Uuid) {
        self.p
            .post(
                &format!("/api/v1/conversations/{id}/stop"),
                Some(json!({"force": false})),
                200,
            )
            .await;
        self.stopped(id).await;
    }

    async fn decide(&self, parent: Uuid, request: Uuid, behavior: &str, message: Value) -> Value {
        self.p
            .post(
                &format!("/api/v1/conversations/{parent}/requests/{request}/decision"),
                Some(json!({"kind": "permission", "behavior": behavior, "message": message})),
                200,
            )
            .await
    }

    async fn finish(self) {
        check_streams(&self.p.pool, self.p.owner, &self.ws).await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn spawn_isolated_commit_summary_accept_cleanup(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let repo = b.repo("repo");
    let orchestrator = b.session("synthetic", &repo).await;
    let brief = script(json!([
        {"git_commit": {"path": "result.txt", "content": "готово\n", "subject": "Добавить итог"}},
        {"delta": "Работа сделана"}
    ]));
    let (child, started) = b
        .spawn_child(
            orchestrator,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief,
                   "title": "Исполнитель", "isolated": true}),
        )
        .await;
    assert_eq!(started["subscribed"], true);
    let branch = started["worktree"]["branch"].as_str().unwrap().to_owned();
    let path = PathBuf::from(started["worktree"]["path"].as_str().unwrap());

    let report = b.report_with(orchestrator, "is now idle").await;
    for needle in [
        "Исполнитель",
        "Turns: 1",
        "Добавить итог",
        "result.txt",
        "Работа сделана",
        &branch,
        "session_accept",
    ] {
        assert!(report.contains(needle), "{needle} is not in the summary");
    }
    assert_eq!(
        git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        branch
    );
    assert!(path.join("result.txt").exists());
    assert!(
        !repo.join("result.txt").exists(),
        "source tree is untouched"
    );

    let (input, _) = b.input_of_event(orchestrator, "is now idle").await;
    b.p.accepted(input).await;
    b.quiet(&[orchestrator, child]).await;

    git(&repo, &["merge", "-q", "--ff-only", &branch]);
    let accepted = b
        .tool(
            orchestrator,
            "sessions.accept",
            json!({"session_id": child}),
        )
        .await
        .expect("accept answers");
    assert_eq!(accepted["status"], "stopping");

    let note = b.event_with(orchestrator, "was accepted").await;
    assert!(note.contains("was deleted"), "branch is merged: {note}");
    let (state, kept, archived): (Option<String>, Option<bool>, bool) = sqlx::query_as(
        "select worktree->>'state',(worktree->>'branch_kept')::boolean,archived_at is not null
         from conversations where id=$1",
    )
    .bind(child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state.as_deref(), Some("removed"));
    assert_eq!(kept, Some(false));
    assert!(archived);
    assert!(!path.exists());
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    assert_eq!(git(&repo, &["branch", "--list", "canterlot/*"]).trim(), "");
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s"]).trim(),
        "Добавить итог"
    );
    assert!(b.p.cli(child).await.is_none(), "child process is gone");
    assert!(
        b.subscriptions(child).await.iter().all(|s| s.2.is_some()),
        "no open subscription"
    );
    b.quiet(&[orchestrator, child]).await;
    assert_eq!(
        b.reports(orchestrator).await.len(),
        1,
        "stop is not a result"
    );
    assert_eq!(
        b.p.detail(orchestrator).await["execution"]["state"],
        "running"
    );
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn dirty_worktree_waits_for_force_and_keeps_unmerged_commits(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let repo = b.repo("repo");
    let orchestrator = b.session("synthetic", &repo).await;
    let brief = script(json!([
        {"git_commit": {"path": "done.txt", "content": "да\n", "subject": "Сделать часть"}},
        {"write_file": {"path": "draft.txt", "content": "черновик\n"}},
        {"delta": "Черновик остался"}
    ]));
    let (child, started) = b
        .spawn_child(
            orchestrator,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief,
                   "isolated": true}),
        )
        .await;
    let branch = started["worktree"]["branch"].as_str().unwrap().to_owned();
    let path = PathBuf::from(started["worktree"]["path"].as_str().unwrap());
    let report = b.report_with(orchestrator, "is now idle").await;
    assert!(report.contains("Uncommitted changes: yes"), "{report}");
    b.quiet(&[orchestrator, child]).await;

    let accepted = b
        .tool(
            orchestrator,
            "sessions.accept",
            json!({"session_id": child}),
        )
        .await
        .expect("accept answers");
    assert_eq!(accepted["status"], "stopping");
    b.event_with(orchestrator, "uncommitted changes").await;
    assert!(path.join("draft.txt").exists(), "nothing is removed");
    b.quiet(&[orchestrator, child]).await;

    let forced = b
        .tool(
            orchestrator,
            "sessions.accept",
            json!({"session_id": child, "force": true}),
        )
        .await
        .expect("accept answers");
    assert_eq!(forced["status"], "removing");
    let note = b.event_with(orchestrator, "was accepted").await;
    assert!(note.contains("was kept"), "{note}");
    assert!(!path.exists());
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s", &branch]).trim(),
        "Сделать часть",
        "commits stay reachable from the kept branch"
    );
    b.quiet(&[orchestrator, child]).await;
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn accept_of_a_working_child_stops_it_and_reports_only_the_removal(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let repo = b.repo("repo");
    let orchestrator = b.session("synthetic", &repo).await;
    let never = repo.join("never");
    let brief = script(json!([{"delta": "Работаю"}, {"wait_file": never}]));
    let (child, started) = b
        .spawn_child(
            orchestrator,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief,
                   "isolated": true}),
        )
        .await;
    let path = PathBuf::from(started["worktree"]["path"].as_str().unwrap());
    until("child works", async || {
        b.p.history(child)
            .await
            .iter()
            .any(|item| item["kind"] == "block" && item["block_kind"] == "text")
    })
    .await;
    let accepted = b
        .tool(
            orchestrator,
            "sessions.accept",
            json!({"session_id": child}),
        )
        .await
        .expect("accept answers");
    assert_eq!(accepted["status"], "stopping");
    b.event_with(orchestrator, "was accepted").await;
    assert!(!path.exists());
    assert_eq!(git(&repo, &["branch", "--list", "canterlot/*"]).trim(), "");
    assert!(b.p.cli(child).await.is_none());
    b.quiet(&[orchestrator, child]).await;
    assert!(
        b.reports(orchestrator).await.is_empty(),
        "accept is not a stop"
    );
    b.finish().await;
}

/// Потомок сам запускает потомка: тот строится от ветки родителя, в той же группе видимости.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn nested_isolated_spawn_builds_on_the_parents_branch(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let repo = b.repo("repo");
    let root = b.session("synthetic", &repo).await;
    let inner = script(json!([
        {"git_commit": {"path": "inner.txt", "content": "2\n", "subject": "Внутренний коммит"}},
        {"delta": "Внутренний итог"}
    ]));
    let brief = script(json!([
        {"git_commit": {"path": "outer.txt", "content": "1\n", "subject": "Внешний коммит"}},
        {"agent_call": {"tool": "sessions.spawn", "arguments": {
            "harness": "synthetic", "model": "synthetic-small", "brief": inner,
            "isolated": true,
        }}},
        {"delta": "Внешний ход"}
    ]));
    let (middle, _) = b
        .spawn_child(
            root,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief,
                   "isolated": true}),
        )
        .await;
    b.report_with(middle, "Внутренний итог").await;
    let (leaf, depth, worktree): (Uuid, i32, Value) = sqlx::query_as(
        "select id,spawn_depth,worktree from conversations where parent_conversation_id=$1",
    )
    .bind(middle)
    .fetch_one(&pool)
    .await
    .unwrap();
    b.watch(leaf).await;
    assert_eq!(depth, 2);
    assert_eq!(worktree["source_workdir"], repo.to_str().unwrap());
    let path = PathBuf::from(worktree["path"].as_str().unwrap());
    assert!(
        path.join("outer.txt").exists(),
        "based on the parent's branch"
    );
    assert!(path.join("inner.txt").exists());
    b.quiet(&[root, middle, leaf]).await;
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn failed_worktree_reaches_the_parent(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let parent = b.session("synthetic", &work).await;
    let answer = b
        .tool(
            parent,
            "sessions.spawn",
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": "Работа",
                   "isolated": true}),
        )
        .await
        .expect("spawn answers");
    assert_eq!(answer["status"], "started", "{answer}");
    let child = uuid(&answer["session_id"]);
    b.watch(child).await;
    let report = b.report_with(parent, "is now failed").await;
    assert!(report.contains("worktree"), "{report}");
    let (input, _) = b.input_of_event(parent, "is now failed").await;
    b.p.accepted(input).await;
    b.finish().await;
}

async fn subscribed_child(b: &mut Bench, orchestrator: Uuid) -> (Uuid, PathBuf) {
    let gate = b.p.work.join(format!("gate-{}", Uuid::now_v7()));
    let brief = script(json!([
        {"delta": "Начало"},
        {"wait_file": gate},
        {"delta": "Итог работы"}
    ]));
    let (child, _) = b
        .spawn_child(
            orchestrator,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief}),
        )
        .await;
    until("child works", async || {
        b.p.history(child)
            .await
            .iter()
            .any(|item| item["kind"] == "block" && item["block_kind"] == "text")
    })
    .await;
    (child, gate)
}

/// Итог приходит к остановленному оркестратору и ждёт его в очереди.
async fn result_waits_for_orchestrator(b: &Bench, orchestrator: Uuid, gate: &Path) {
    fs::write(gate, "").unwrap();
    b.report_with(orchestrator, "Итог работы").await;
    let (input, state) = b.input_of_event(orchestrator, "Итог работы").await;
    assert_eq!(state, "queued", "nothing to deliver it to");
    b.resume(orchestrator).await;
    b.p.accepted(input).await;
    assert_eq!(b.reports(orchestrator).await.len(), 1);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn subscription_survives_orchestrator_restart(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let orchestrator = b.session("synthetic", &work).await;
    let (_child, gate) = subscribed_child(&mut b, orchestrator).await;
    b.stop(orchestrator).await;
    result_waits_for_orchestrator(&b, orchestrator, &gate).await;
    b.quiet(&[orchestrator]).await;
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn subscription_survives_killed_orchestrator(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let orchestrator = b.session("synthetic", &work).await;
    let (_child, gate) = subscribed_child(&mut b, orchestrator).await;
    let cli = b.p.cli(orchestrator).await.expect("orchestrator CLI");
    assert!(b.p.guard.signal(cli, Signal::KILL));
    until("orchestrator execution ended", async || {
        b.p.detail(orchestrator).await["execution"]["state"] != "running"
    })
    .await;
    b.stopped(orchestrator).await;
    result_waits_for_orchestrator(&b, orchestrator, &gate).await;
    b.quiet(&[orchestrator]).await;
    b.finish().await;
}

/// Двойник Claude Code зовёт `session_spawn` через настоящий MCP: процесс-ретранслятор, брокер
/// адаптера и сопровождение. Владелец решает по HTTP, итог приходит родителю обычным вводом.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn approval_end_to_end_over_http_and_mcp(pool: PgPool) {
    let cli = tempfile::tempdir().unwrap();
    let executable = cli.path().join("fake-cli");
    fake_exec::write_executable(&executable, include_str!("support/fake_claude.py"), 0o700);
    fs::write(
        cli.path().join("relay-exe"),
        env!("CARGO_BIN_EXE_canterlot-worker"),
    )
    .unwrap();
    let brief = script(json!([{"delta": "Сделано после одобрения"}]));
    let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
        "name": "session_spawn",
        "arguments": {"harness": "synthetic", "model": "synthetic-small", "brief": brief},
    }});
    fs::write(cli.path().join("mcp-calls.json"), json!([call]).to_string()).unwrap();
    let answer = |extra: &[Value]| {
        let start = json!({"type": "stream_event", "parent_tool_use_id": null,
            "user_message_uuids": ["@input"], "event": {"type": "message_start",
            "message": {"id": "m1", "model": "claude-haiku-test", "usage": {}}}});
        let finish = json!({"type": "result", "subtype": "success", "is_error": false,
            "user_message_uuids": ["@input"], "modelUsage": {}});
        let mut frames = vec![start];
        frames.extend_from_slice(extra);
        frames.push(finish);
        Value::Array(frames).to_string()
    };
    // Номер хода считает и управляющие кадры, поэтому файлы есть на всю длину сценария.
    for turn in 1..=20 {
        fs::write(cli.path().join(format!("turn{turn}.json")), answer(&[])).unwrap();
    }
    let mut options = options();
    options.claude_code = Some(ClaudeCodeConfig {
        executable,
        ..Default::default()
    });
    let mut b = Bench::with(&pool, options).await;
    b.whitelist(json!([])).await;
    let work = b.p.work.clone();
    let parent = b.session_on("claude_code", "haiku", "low", &work).await;
    let stdin = cli.path().join("stdin.log");
    let turns = fs::read_to_string(&stdin).map_or(0, |log| log.lines().count());
    fs::write(
        cli.path().join(format!("turn{}.json", turns + 1)),
        answer(&[json!("@request:0")]),
    )
    .unwrap();
    let submitted = b.p.submit(parent, "Запусти помощника", &[]).await;
    b.p.accepted(uuid(&submitted["input_id"])).await;

    let replies = cli.path().join("mcp-replies.jsonl");
    until("MCP answers", async || replies.exists()).await;
    let reply: Value = serde_json::from_str(fs::read_to_string(&replies).unwrap().trim()).unwrap();
    assert_eq!(reply["id"], 7);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pending_approval"), "{text}");
    assert!(text.contains("The owner decides in the app"), "{text}");
    let (request, kind): (Uuid, String) =
        sqlx::query_as("select id,kind from requests where conversation_id=$1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind, "spawn_approval");
    b.ws.wait_event(
        "card over WSS",
        &format!("conversation:{parent}"),
        "request_opened",
        |r| r["request_id"] == request.to_string(),
    )
    .await;
    b.decide(parent, request, "allow", Value::Null).await;

    let report = b.report_with(parent, "Сделано после одобрения").await;
    let (input, _) = b.input_of_event(parent, "Сделано после одобрения").await;
    b.p.accepted(input).await;
    let delivered = fs::read_to_string(&stdin).unwrap();
    assert!(
        delivered.contains("is now idle"),
        "the parent CLI reads the report"
    );
    assert!(report.contains("synthetic-small"), "{report}");
    let child: Uuid =
        sqlx::query_scalar("select id from conversations where parent_conversation_id=$1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    b.watch(child).await;
    b.quiet(&[parent, child]).await;
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn approval_card_holds_nothing_and_a_refusal_starts_nothing(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    b.whitelist(json!([])).await;
    let work = b.p.work.clone();
    let parent = b.session("synthetic", &work).await;

    let brief = script(json!([{"delta": "Сделано после одобрения"}]));
    let pending = b
        .tool(
            parent,
            "sessions.spawn",
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief}),
        )
        .await
        .expect("spawn answers");
    assert_eq!(pending["status"], "pending_approval");
    let request = uuid(&pending["request_id"]);
    b.ws.wait_event(
        "card over WSS",
        &format!("conversation:{parent}"),
        "request_opened",
        |r| r["kind"] == "spawn_approval" && r["request_id"] == request.to_string(),
    )
    .await;
    let snapshot =
        b.p.get(&format!("/api/v1/conversations/{parent}/snapshot"))
            .await;
    assert!(
        snapshot["data"]["open_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == request.to_string()),
        "owner sees the card after a reload"
    );
    // Карточка не держит ни очередь, ни активность родителя.
    b.quiet(&[parent]).await;
    // Активность — производное состояние: хаб пересчитывает его после уведомления о конце
    // хода, позже освобождения очереди. `working` здесь — только хвост хода с вызовом
    // инструмента. Карточка открыта внутри хода, поэтому любой пересчёт после него её
    // уже видит: держи она активность, состояние застряло бы в `waiting_for_user`.
    let mut activity = Value::Null;
    until("activity leaves the spawn turn", async || {
        activity = b.p.detail(parent).await["activity"]["state"].clone();
        activity != "working"
    })
    .await;
    assert_eq!(activity, "idle", "the card is not a question");
    b.tool(parent, "sessions.list", json!({}))
        .await
        .expect("parent keeps working");

    assert_eq!(
        b.decide(parent, request, "allow", Value::Null).await["state"],
        "decision_saved"
    );
    let started = b.event_with(parent, "started").await;
    assert!(started.contains("[canterlot]"), "{started}");
    let child: Uuid =
        sqlx::query_scalar("select id from conversations where parent_conversation_id=$1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    b.watch(child).await;
    b.report_with(parent, "Сделано после одобрения").await;
    let state: String = sqlx::query_scalar("select state from requests where id=$1")
        .bind(request)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "decision_applied");

    let brief = script(json!([{"delta": "Этого не будет"}]));
    let refused = b
        .tool(
            parent,
            "sessions.spawn",
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief}),
        )
        .await
        .expect("spawn answers");
    assert_eq!(refused["status"], "pending_approval");
    b.decide(
        parent,
        uuid(&refused["request_id"]),
        "deny",
        json!("не сейчас"),
    )
    .await;
    b.event_with(parent, "не сейчас").await;
    let children: i64 =
        sqlx::query_scalar("select count(*) from conversations where parent_conversation_id=$1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(children, 1, "a refused card starts nothing");
    b.quiet(&[parent, child]).await;
    b.finish().await;
}

async fn harness_reaches_the_tools(pool: PgPool, harness: &str) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let parent = b.session(harness, &work).await;
    let listed = b
        .tool(parent, "sessions.list", json!({}))
        .await
        .expect(harness);
    assert!(
        listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == parent.to_string() && s["is_self"] == true),
        "{harness}"
    );
    let brief = script(json!([{"delta": "Первый итог"}]));
    let (child, spawned) = b
        .spawn_child(
            parent,
            json!({"harness": harness, "model": "synthetic-small", "brief": brief}),
        )
        .await;
    assert_eq!(spawned["permission_mode"], "owner", "{harness}");
    b.report_with(parent, "Первый итог").await;

    let inspected = b
        .tool(parent, "sessions.inspect", json!({"session_id": child}))
        .await
        .expect(harness);
    assert_eq!(inspected["activity"]["state"], "idle", "{harness}");
    let subscribed = b
        .tool(parent, "sessions.subscribe", json!({"session_id": child}))
        .await
        .expect(harness);
    assert_eq!(subscribed["subscribed"], true);
    assert_eq!(subscribed["current_state"], "idle");
    let unsubscribed = b
        .tool(parent, "sessions.unsubscribe", json!({"session_id": child}))
        .await
        .expect(harness);
    assert_eq!(unsubscribed["subscribed"], false);
    let sent = b
        .tool(
            parent,
            "sessions.send",
            json!({"session_id": child, "text": "Продолжай"}),
        )
        .await
        .expect(harness);
    assert_eq!(sent["subscribed"], true);
    let waited = b
        .tool(
            parent,
            "sessions.wait",
            json!({"session_id": child, "timeout_seconds": 20}),
        )
        .await
        .expect(harness);
    assert_eq!(waited["state"], "idle", "{harness}");
    assert_eq!(waited["timed_out"], false);
    b.quiet(&[parent, child]).await;
    let expected = if waited["event_suppressed"] == true {
        1
    } else {
        2
    };
    assert_eq!(b.reports(parent).await.len(), expected, "{harness}");
    let accepted = b
        .tool(parent, "sessions.accept", json!({"session_id": child}))
        .await
        .expect(harness);
    assert_eq!(accepted["status"], "stopping", "{harness}");
    b.stopped(child).await;
    b.event_with(parent, "was accepted").await;

    // Режим владельца не переходит через границу харнесса.
    let other = HARNESSES[(HARNESSES.iter().position(|h| *h == harness).unwrap() + 1) % 3];
    let (crossed, answer) = b
        .spawn_child(
            parent,
            json!({"harness": other, "model": "synthetic-small", "brief": "Другой харнесс"}),
        )
        .await;
    assert_eq!(answer["permission_mode"], "ask", "{harness} to {other}");
    b.quiet(&[parent, crossed]).await;
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn synthetic_harness_name_reaches_the_tools(pool: PgPool) {
    harness_reaches_the_tools(pool, "synthetic").await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn exit_on_interrupt_harness_name_reaches_the_tools(pool: PgPool) {
    harness_reaches_the_tools(pool, "synthetic-exit-on-interrupt").await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn codex_harness_name_reaches_the_tools(pool: PgPool) {
    harness_reaches_the_tools(pool, "synthetic-codex").await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn stop_of_child_notifies_parent(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let parent = b.session("synthetic", &work).await;
    let (child, _gate) = subscribed_child(&mut b, parent).await;
    b.stop(child).await;
    let report = b.report_with(parent, "is now stopped").await;
    assert!(report.contains("Начало"), "last words of the child");
    let (input, _) = b.input_of_event(parent, "is now stopped").await;
    b.p.accepted(input).await;
    assert_eq!(
        b.subscriptions(child).await,
        vec![(parent, child, Some("stopped".to_owned()))]
    );
    let accepted = b
        .tool(parent, "sessions.accept", json!({"session_id": child}))
        .await
        .expect("accept answers");
    assert_eq!(accepted["status"], "done");
    b.quiet(&[parent, child]).await;
    assert_eq!(b.reports(parent).await.len(), 1);
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn loop_is_stopped_end_to_end(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let first = b.session("synthetic", &work).await;
    let second = b.session("synthetic", &work).await;
    // Пинг-понг сообщениями: каждый ход отвечает другой сессии, цепочка растёт на каждом ходу.
    let hops: Vec<PathBuf> = (0..=10)
        .map(|n| work.join(format!("hop-{n}.json")))
        .collect();
    for (n, path) in hops.iter().enumerate().take(10) {
        let target = if n % 2 == 0 { second } else { first };
        let steps = json!([send_step(target, &hop(&hops[n + 1]), true), {"delta": "Отвечаю"}]);
        fs::write(path, steps.to_string()).unwrap();
    }
    fs::write(&hops[10], "[]").unwrap();
    b.p.submit(first, &hop(&hops[0]), &[]).await;
    // Глубина 8 проходит, девятое сообщение из хода глубины 8 получает отказ.
    until("chain refused", async || {
        b.results(first)
            .await
            .iter()
            .any(|result| result.starts_with("chain_depth_exceeded"))
    })
    .await;
    b.quiet(&[first, second]).await;
    sleep(QUIET * 3).await;
    b.quiet(&[first, second]).await;
    let mut depths: Vec<i32> = sqlx::query_scalar(
        "select (origin->>'chain_depth')::int from inputs
         where conversation_id=any($1) and origin->>'kind'='message'",
    )
    .bind([first, second])
    .fetch_all(&pool)
    .await
    .unwrap();
    depths.sort_unstable();
    assert_eq!(depths, (1..=8).collect::<Vec<_>>());
    for id in [first, second] {
        assert_eq!(b.p.detail(id).await["execution"]["state"], "running");
    }
    b.finish().await;
}

/// D16: событие несёт глубину подписки, поэтому после него цепочка считается заново. Цикл через
/// события глубиной не ловится, его держит предел частоты пары.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn event_after_message_lowers_depth_and_the_rate_limit_stops_the_loop(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let first = b.session("synthetic", &work).await;
    let second = b.session("synthetic", &work).await;
    const CYCLES: usize = 11;
    let files = |name: &str| -> Vec<PathBuf> {
        (0..=CYCLES)
            .map(|n| work.join(format!("{name}-{n}.json")))
            .collect()
    };
    let (sends, replies, acks) = (files("send"), files("reply"), files("ack"));
    for n in 0..CYCLES {
        let steps = json!([send_step(second, &hop(&replies[n]), true)]);
        fs::write(&sends[n], steps.to_string()).unwrap();
        // Ответ сообщением и итог хода: первый увидит сообщение, а за ним событие.
        let steps = json!([
            send_step(first, &hop(&acks[n]), false),
            {"delta": hop(&sends[n + 1])}
        ]);
        fs::write(&replies[n], steps.to_string()).unwrap();
        fs::write(&acks[n], json!([{"delta": "Принято"}]).to_string()).unwrap();
    }
    fs::write(&sends[CYCLES], "[]").unwrap();
    b.p.submit(first, &hop(&sends[0]), &[]).await;
    until("pair limit refused", async || {
        b.results(first)
            .await
            .iter()
            .any(|result| result.starts_with("rate_limited"))
    })
    .await;
    b.quiet(&[first, second]).await;
    let depths: Vec<i32> = sqlx::query_scalar(
        "select (origin->>'chain_depth')::int from inputs
         where conversation_id=$1 and origin->>'kind'='message' order by input_seq",
    )
    .bind(second)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        depths,
        vec![1; 10],
        "an event resets the chain, as D16 reads"
    );
    b.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn send_to_a_stopped_target_is_heard_after_the_owner_resumes_it(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let sender = b.session("synthetic", &work).await;
    let target = b.session("synthetic", &work).await;
    b.stop(target).await;
    let sent = b
        .tool(
            sender,
            "sessions.send",
            json!({"session_id": target, "text": script(json!([{"delta": "Ответ после возврата"}]))}),
        )
        .await
        .expect("send answers");
    assert_eq!(sent["subscribed"], true);
    assert_eq!(sent["target_stopped"], true);
    let stopped = b.report_with(sender, "is now stopped").await;
    assert!(!stopped.contains("Ответ после возврата"));
    b.quiet(&[sender]).await;
    sleep(QUIET * 2).await;
    assert_eq!(b.reports(sender).await.len(), 1, "one report per state");

    b.resume(target).await;
    b.report_with(sender, "Ответ после возврата").await;
    b.quiet(&[sender, target]).await;
    assert_eq!(b.reports(sender).await.len(), 2);
    assert_eq!(
        b.subscriptions(target).await,
        vec![(sender, target, Some("idle".to_owned()))]
    );
    b.finish().await;
}

/// Фон кончился намного позже хода, ход пробуждения начался сразу за ним: окно тишины должно
/// отсчитываться от конца фона, иначе родитель получит итог первого хода и оглохнет.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wake_turn_right_after_background_is_the_reported_result(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let parent = b.session("synthetic", &work).await;
    let brief = script(json!([
        background("running"),
        {"delta": "Запустила фон"},
        {"after_turn": [
            {"delay_ms": QUIET.as_millis() as u64 * 3},
            background("completed"),
            {"delay_ms": QUIET.as_millis() as u64 / 2},
            {"adopt": true},
            {"delta": "Итог после фона"}
        ]}
    ]));
    let (child, _) = b
        .spawn_child(
            parent,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief}),
        )
        .await;
    let report = b.report_with(parent, "is now idle").await;
    assert!(report.contains("Итог после фона"), "{report}");
    b.quiet(&[parent, child]).await;
    sleep(QUIET * 2).await;
    assert_eq!(b.reports(parent).await.len(), 1);
    b.finish().await;
}

/// Ход пробуждения начался уже после окна тишины: первый итог ушёл, и второй не должен пропасть.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wake_turn_after_the_quiet_window_is_still_heard(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let parent = b.session("synthetic", &work).await;
    let gate = work.join("wake-gate");
    let brief = script(json!([
        background("running"),
        {"delta": "Запустила фон"},
        {"after_turn": [
            background("completed"),
            {"wait_file": gate},
            {"adopt": true},
            {"delay_ms": 300},
            {"delta": "Итог после фона"}
        ]}
    ]));
    let (child, _) = b
        .spawn_child(
            parent,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief}),
        )
        .await;
    b.report_with(parent, "Запустила фон").await;
    b.quiet(&[parent, child]).await;
    fs::write(&gate, "").unwrap();
    b.report_with(parent, "Итог после фона").await;
    b.quiet(&[parent, child]).await;
    assert_eq!(b.reports(parent).await.len(), 2);
    b.finish().await;
}

/// Отложенная смена инструкций перезапускает CLI после фона, без ввода. Сессия остаётся той же:
/// итог один, следующее сообщение доходит до нового процесса.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn deferred_restart_keeps_the_session_and_its_subscription(pool: PgPool) {
    let mut b = Bench::new(&pool).await;
    let work = b.p.work.clone();
    let parent = b.session("synthetic", &work).await;
    let gate = work.join("restart-gate");
    let brief = script(json!([
        background("running"),
        {"delta": "Запустила фон"},
        {"after_turn": [
            {"wait_file": gate},
            background("completed"),
            {"restart_due": true, "restart": true}
        ]}
    ]));
    let (child, _) = b
        .spawn_child(
            parent,
            json!({"harness": "synthetic", "model": "synthetic-small", "brief": brief}),
        )
        .await;
    until("first CLI started", async || b.p.cli(child).await.is_some()).await;
    // Без затвора перезапуск мог успеть до этого чтения, и «первым» оказался бы уже новый CLI.
    let first = b.p.cli(child).await.unwrap();
    fs::write(&gate, "").unwrap();
    until("deferred restart applied", async || {
        b.p.cli(child).await.is_some_and(|cli| cli != first)
    })
    .await;
    b.report_with(parent, "Запустила фон").await;
    b.quiet(&[parent, child]).await;
    let sent = b
        .tool(
            parent,
            "sessions.send",
            json!({"session_id": child, "text": "Ещё один ход"}),
        )
        .await
        .expect("send answers");
    assert_eq!(sent["target_stopped"], false);
    b.report_with(parent, "Ещё один ход").await;
    b.quiet(&[parent, child]).await;
    assert_eq!(b.reports(parent).await.len(), 2);
    assert_eq!(b.p.detail(child).await["execution"]["state"], "running");
    b.finish().await;
}
