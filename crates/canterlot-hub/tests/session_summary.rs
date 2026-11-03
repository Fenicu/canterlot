use canterlot_hub::{
    activity::{self, ActivityState as S, Evidence as E, Thresholds},
    executions,
    model::ExecutionState,
    requests::{self, RequestKind},
    session_tools::summary,
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

struct Fixture {
    owner: Uuid,
    machine: Uuid,
    target: Uuid,
    execution: executions::StartedExecution,
}

async fn fixture(pool: &PgPool) -> Fixture {
    let s = testkit::seed(pool).await;
    let execution = executions::start(pool, s.owner_id, s.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query("update conversations set title='Build parser', harness='claude_code', model='sonnet' where id=$1")
        .bind(s.conversation_id)
        .execute(pool)
        .await
        .unwrap();
    Fixture {
        owner: s.owner_id,
        machine: s.machine_id,
        target: s.conversation_id,
        execution,
    }
}

struct Run {
    finished_at: DateTime<Utc>,
    duration_ms: i64,
    git: Option<Value>,
    tokens: (i64, i64),
}

async fn add_run(pool: &PgPool, f: &Fixture, run: Run) -> Uuid {
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(f.target)
    .bind(f.execution.execution_id)
    .bind(f.execution.generation)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms,git)
         values($1,$2,$3,$4-($5::bigint * interval '1 millisecond'),$4,$5,$6)",
    )
    .bind(command)
    .bind(f.execution.execution_id)
    .bind(f.target)
    .bind(run.finished_at)
    .bind(run.duration_ms)
    .bind(run.git)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens)
         values($1,'sonnet','main',$2,$3,7,7)",
    )
    .bind(command)
    .bind(run.tokens.0)
    .bind(run.tokens.1)
    .execute(pool)
    .await
    .unwrap();
    command
}

async fn add_text(
    pool: &PgPool,
    f: &Fixture,
    seq: i64,
    command: Option<Uuid>,
    parent: Option<Uuid>,
    text: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,input_command_id,parent_block_id,created_seq)
         values($1,$2,$3,'text','final',1,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(f.execution.execution_id)
    .bind(f.target)
    .bind(text)
    .bind(command)
    .bind(parent)
    .bind(seq)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn git(
    before: &str,
    after: &str,
    dirty: bool,
    commits: &[(&str, &str)],
    files: &[(&str, &str)],
) -> Value {
    json!({
        "available": true, "head_before": before, "head_after": after, "dirty": dirty,
        "truncated": false,
        "commits": commits.iter().map(|(sha, subject)| json!({"sha": sha, "subject": subject})).collect::<Vec<_>>(),
        "files": files.iter().map(|(path, status)| json!({"path": path, "status": status})).collect::<Vec<_>>(),
    })
}

async fn text_for(
    pool: &PgPool,
    f: &Fixture,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
) -> String {
    let mut conn = pool.acquire().await.unwrap();
    summary::event_text(&mut conn, f.owner, f.target, state, armed_at)
        .await
        .unwrap()
        .expect("target exists")
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_covers_state_answer_git_usage_duration(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let command = add_run(
        &pool,
        &f,
        Run {
            finished_at: now,
            duration_ms: 252_000,
            git: Some(git(
                "1a2b3c4d5e6f",
                "5d6e7f8a9b0c",
                true,
                &[("5d6e7f8a9b0c", "Parse the header")],
                &[("src/a.rs", "M"), ("src/b.rs", "A")],
            )),
            tokens: (12345, 678),
        },
    )
    .await;
    add_text(&pool, &f, 1, Some(command), None, "Early note").await;
    add_text(&pool, &f, 2, Some(command), None, "All done, tests pass.").await;
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(f.target)
        .bind(json!({"source_workdir":"/w","path":"/data/worktrees/x","branch":"canterlot/0190abcd","base":"HEAD","state":"active"}))
        .execute(&pool)
        .await
        .unwrap();

    let text = text_for(&pool, &f, "idle", Some(now - TimeDelta::minutes(10))).await;
    let expected = format!(
        "[canterlot] Session \"Build parser\" (id {}, claude_code, model sonnet) is now idle.\n\
         Turns: 1, duration 4m12s, tokens in/out 12345/678.\n\
         HEAD 1a2b3c4 -> 5d6e7f8, 1 commit:\n\
         - 5d6e7f8 Parse the header\n\
         Changed files (2):\n\
         M src/a.rs\n\
         A src/b.rs\n\
         Uncommitted changes: yes\n\
         Worktree: /data/worktrees/x, branch canterlot/0190abcd.\n\
         Final answer:\n\
         All done, tests pass.\n\
         \n\
         Next: read the result, then call session_accept to clean up, or session_send to continue.",
        f.target
    );
    assert_eq!(text, expected);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_aggregates_several_turns(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let armed = now - TimeDelta::minutes(30);
    let before_window = add_run(
        &pool,
        &f,
        Run {
            finished_at: armed - TimeDelta::minutes(5),
            duration_ms: 999_000,
            git: Some(git(
                "0000000",
                "1111111",
                false,
                &[("1111111", "Old")],
                &[("old.rs", "M")],
            )),
            tokens: (1_000_000, 1_000_000),
        },
    )
    .await;
    add_text(&pool, &f, 1, Some(before_window), None, "Old answer").await;
    let first = add_run(
        &pool,
        &f,
        Run {
            finished_at: armed + TimeDelta::minutes(5),
            duration_ms: 60_000,
            git: Some(git(
                "1111111aaa",
                "2222222bbb",
                true,
                &[("2222222bbb", "First")],
                &[("a.rs", "A"), ("b.rs", "M")],
            )),
            tokens: (100, 10),
        },
    )
    .await;
    let second = add_run(
        &pool,
        &f,
        Run {
            finished_at: armed + TimeDelta::minutes(10),
            duration_ms: 125_000,
            git: Some(git(
                "2222222bbb",
                "3333333ccc",
                false,
                &[("3333333ccc", "Second"), ("2222222bbb", "First")],
                &[("b.rs", "D"), ("c.rs", "A")],
            )),
            tokens: (200, 20),
        },
    )
    .await;
    add_text(&pool, &f, 2, Some(first), None, "First answer").await;
    add_text(&pool, &f, 3, Some(second), None, "Second answer").await;
    // Блок подагента (есть родитель) позднее по порядку, но итоговым ответом не считается.
    add_text(
        &pool,
        &f,
        4,
        Some(second),
        Some(Uuid::now_v7()),
        "Helper chatter",
    )
    .await;

    let text = text_for(&pool, &f, "idle", Some(armed)).await;
    let l = lines(&text);
    assert_eq!(l[1], "Turns: 2, duration 3m05s, tokens in/out 300/30.");
    assert_eq!(l[2], "HEAD 1111111 -> 3333333, 2 commits:");
    assert_eq!(l[3], "- 3333333 Second");
    assert_eq!(l[4], "- 2222222 First");
    assert_eq!(l[5], "Changed files (3):");
    assert_eq!(&l[6..9], ["A a.rs", "D b.rs", "A c.rs"]);
    assert_eq!(l[9], "Uncommitted changes: no");
    assert!(text.contains("Final answer:\nSecond answer\n"), "{text}");
    assert!(!text.contains("Old answer") && !text.contains("Helper chatter"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_working_time_skips_gaps_between_turns(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    add_run(
        &pool,
        &f,
        Run {
            finished_at: now - TimeDelta::minutes(20),
            duration_ms: 60_000,
            git: None,
            tokens: (1, 1),
        },
    )
    .await;
    add_run(
        &pool,
        &f,
        Run {
            finished_at: now,
            duration_ms: 125_000,
            git: None,
            tokens: (1, 1),
        },
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();
    let summary = summary::build(&mut conn, f.owner, f.target, "idle", None)
        .await
        .unwrap()
        .unwrap();
    let event = summary.hub_event();
    assert_eq!(event.duration_ms, Some(185_000));
    assert_eq!(
        event.working_ms,
        Some(185_000),
        "the idle gap between turns is not working time"
    );
}

async fn add_wait(
    pool: &PgPool,
    f: &Fixture,
    kind: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload,created_at,updated_at)
         values($1,$2,$3,$4,$5,$6,'decision_applied','{}',$7,$8)",
    )
    .bind(id)
    .bind(f.target)
    .bind(f.execution.execution_id)
    .bind(f.execution.generation)
    .bind(kind)
    .bind(format!("{kind}:{id}"))
    .bind(from)
    .bind(to)
    .execute(pool)
    .await
    .unwrap();
    id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_working_time_subtracts_permission_and_question_waits(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let finished = now - TimeDelta::minutes(1);
    let started = finished - TimeDelta::minutes(10);
    add_run(
        &pool,
        &f,
        Run {
            finished_at: finished,
            duration_ms: 600_000,
            git: None,
            tokens: (1, 1),
        },
    )
    .await;
    add_wait(
        &pool,
        &f,
        "permission",
        started + TimeDelta::minutes(1),
        started + TimeDelta::minutes(4),
    )
    .await;
    add_wait(
        &pool,
        &f,
        "question",
        started + TimeDelta::minutes(5),
        started + TimeDelta::minutes(8),
    )
    .await;
    // Одобрение запуска ждёт родитель, а не ход этой сессии.
    add_wait(
        &pool,
        &f,
        "spawn_approval",
        started + TimeDelta::minutes(8),
        started + TimeDelta::minutes(9),
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();
    let summary = summary::build(&mut conn, f.owner, f.target, "idle", None)
        .await
        .unwrap()
        .unwrap();
    let event = summary.hub_event();
    assert_eq!(event.duration_ms, Some(600_000));
    assert_eq!(
        event.working_ms,
        Some(240_000),
        "six minutes of owner waits are not working time"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_truncates_on_char_boundary(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let command = add_run(
        &pool,
        &f,
        Run {
            finished_at: now,
            duration_ms: 1000,
            git: None,
            tokens: (1, 1),
        },
    )
    .await;
    // Три байта на символ: 8192 не делится на три, граница уходит на 8190.
    let answer = "€".repeat(4000);
    let block = add_text(&pool, &f, 1, Some(command), None, &answer).await;

    let text = text_for(&pool, &f, "idle", Some(now - TimeDelta::minutes(1))).await;
    let header = format!(
        "Final answer (truncated to 8190 bytes of 12000; session_inspect with block_id {block} and offset 8190 reads on):\n"
    );
    let start = text.find(&header).expect("truncation header") + header.len();
    let body = text[start..].split("\n\nNext:").next().unwrap();
    assert_eq!(body.len(), 8190);
    assert!(body.chars().all(|c| c == '€'));
    assert!(text.len() <= 16 * 1024);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_whole_text_is_capped(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let commits: Vec<(String, String)> = (0..50)
        .map(|i| (format!("{i:040x}"), "s".repeat(256)))
        .collect();
    let files: Vec<(String, String)> = (0..200)
        .map(|i| (format!("{i:04}{}", "p".repeat(1020)), "M".to_owned()))
        .collect();
    let facts = json!({
        "available": true, "head_before": "a".repeat(40), "head_after": "b".repeat(40),
        "dirty": true, "truncated": true,
        "commits": commits.iter().map(|(s, m)| json!({"sha": s, "subject": m})).collect::<Vec<_>>(),
        "files": files.iter().map(|(p, s)| json!({"path": p, "status": s})).collect::<Vec<_>>(),
    });
    let command = add_run(
        &pool,
        &f,
        Run {
            finished_at: now,
            duration_ms: 1000,
            git: Some(facts),
            tokens: (1, 1),
        },
    )
    .await;
    add_text(&pool, &f, 1, Some(command), None, &"x".repeat(20_000)).await;
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(f.target)
        .bind(json!({"path": "w".repeat(1024), "branch": "canterlot/0190abcd", "state": "active"}))
        .execute(&pool)
        .await
        .unwrap();

    let text = text_for(&pool, &f, "idle", Some(now - TimeDelta::minutes(1))).await;
    assert!(text.len() <= 16 * 1024, "{}", text.len());
    assert!(
        text.ends_with("or session_send to continue."),
        "footer must survive"
    );
    assert!(text.contains("Final answer (truncated to"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_without_git(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let armed = now - TimeDelta::minutes(10);
    for git in [None, Some(json!({"available": false}))] {
        sqlx::query("delete from runs where conversation_id=$1")
            .bind(f.target)
            .execute(&pool)
            .await
            .unwrap();
        add_run(
            &pool,
            &f,
            Run {
                finished_at: now,
                duration_ms: 5000,
                git,
                tokens: (3, 4),
            },
        )
        .await;
        let text = text_for(&pool, &f, "idle", Some(armed)).await;
        assert!(text.contains("\nGit: unavailable\n"), "{text}");
        assert!(!text.contains("HEAD "), "{text}");
        assert!(!text.contains("Changed files") && !text.contains("Uncommitted"));
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_running_turn_reports_its_own_turn_and_duration(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(f.target)
    .bind(f.execution.execution_id)
    .bind(f.execution.generation)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at)
         values($1,$2,$3,$4)",
    )
    .bind(command)
    .bind(f.execution.execution_id)
    .bind(f.target)
    .bind(now - TimeDelta::minutes(5))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(f.target)
        .bind(json!({"source_workdir":"/w","path":"/data/worktrees/x","branch":"canterlot/0190abcd","base":"HEAD","state":"active"}))
        .execute(&pool)
        .await
        .unwrap();

    let text = text_for(&pool, &f, "stalled", None).await;
    assert!(text.contains("Turns: 1, duration "), "{text}");
    assert!(!text.contains("Turns: 0"), "{text}");
    assert!(!text.contains("duration 0s"), "{text}");
    assert!(text.contains("tokens in/out unknown."), "{text}");
    assert!(!text.contains("Git: unavailable"), "{text}");
    assert!(text.contains("Git: no finished turn yet."), "{text}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn running_turn_keeps_git_facts_from_the_last_finished_turn(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    add_run(
        &pool,
        &f,
        Run {
            finished_at: now - TimeDelta::minutes(20),
            duration_ms: 60_000,
            git: Some(git(
                "1a2b3c4d5e6f",
                "5d6e7f8a9b0c",
                false,
                &[("5d6e7f8a9b0c", "Parse the header")],
                &[],
            )),
            tokens: (100, 20),
        },
    )
    .await;
    let open = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(open)
    .bind(f.machine)
    .bind(f.target)
    .bind(f.execution.execution_id)
    .bind(f.execution.generation)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at)
         values($1,$2,$3,$4)",
    )
    .bind(open)
    .bind(f.execution.execution_id)
    .bind(f.target)
    .bind(now - TimeDelta::minutes(3))
    .execute(&pool)
    .await
    .unwrap();

    let text = text_for(&pool, &f, "stalled", Some(now - TimeDelta::minutes(5))).await;
    assert!(text.contains("Turns: 1, duration "), "{text}");
    assert!(text.contains("HEAD 1a2b3c4 -> 5d6e7f8"), "{text}");
    assert!(!text.contains("Git: unavailable"), "{text}");
}

async fn set_activity(pool: &PgPool, id: Uuid, state: S, evidence: E) {
    let thresholds = Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
    };
    activity::refresh(pool, id, Utc::now(), &thresholds, Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query("update conversation_derived set activity_state=$2,activity_evidence=$3 where conversation_id=$1")
        .bind(id)
        .bind(serde_json::to_value(state).unwrap().as_str().unwrap().to_owned())
        .bind(serde_json::to_value(evidence).unwrap())
        .execute(pool)
        .await
        .unwrap();
}

async fn open_question(pool: &PgPool, f: &Fixture, payload: Value) -> Uuid {
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await.unwrap();
    requests::create_request_in_tx(
        &mut tx,
        &requests::NewRequest {
            id,
            conversation_id: f.target,
            execution_id: f.execution.execution_id,
            generation: f.execution.generation,
            kind: RequestKind::Question,
            native_request_id: format!("q:{id}"),
            request_payload: payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_variants_for_each_state(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let armed = Some(now - TimeDelta::minutes(10));
    let header = |state: &str| {
        format!(
            "[canterlot] Session \"Build parser\" (id {}, claude_code, model sonnet) is now {state}.",
            f.target
        )
    };

    open_question(
        &pool,
        &f,
        json!({"questions": [{"question": "Which database should I use?"}]}),
    )
    .await;
    set_activity(
        &pool,
        f.target,
        S::WaitingForUser,
        E::WaitingForUser {
            phase: "awaiting_answer".into(),
            question: None,
        },
    )
    .await;
    let text = text_for(&pool, &f, "waiting_for_user", armed).await;
    assert!(text.starts_with(&header("waiting_for_user")), "{text}");
    assert!(
        text.contains("Question: Which database should I use?"),
        "{text}"
    );
    assert!(text.contains("The owner answers in the app; do not answer for them."));
    assert!(
        !text.contains("session_accept"),
        "no cleanup advice while it waits: {text}"
    );

    let at = now - TimeDelta::minutes(7);
    set_activity(
        &pool,
        f.target,
        S::Stalled,
        E::Stalled {
            last_event_at: at,
            phase: "tool".into(),
            process_alive: Some(true),
            cpu_busy: Some(false),
            link: "available".into(),
        },
    )
    .await;
    let text = text_for(&pool, &f, "stalled", armed).await;
    assert!(text.starts_with(&header("stalled")));
    assert!(text.contains("Phase: tool. Last event: "), "{text}");
    assert!(text.contains("Process alive: yes. CPU busy: no."), "{text}");

    set_activity(
        &pool,
        f.target,
        S::Failed,
        E::Failed {
            reason: "machine_rebooted".into(),
        },
    )
    .await;
    let text = text_for(&pool, &f, "failed", armed).await;
    assert!(text.starts_with(&header("failed")));
    assert!(text.contains("Reason: machine_rebooted."), "{text}");

    set_activity(
        &pool,
        f.target,
        S::QuotaPaused,
        E::QuotaPaused {
            reason: "limit".into(),
            provider: Some("anthropic".into()),
            window: Some("weekly".into()),
            until: Some(now + TimeDelta::hours(3)),
        },
    )
    .await;
    let text = text_for(&pool, &f, "quota_paused", armed).await;
    assert!(text.starts_with(&header("quota_paused")));
    assert!(
        text.contains("Provider: anthropic. Window: weekly. Until: "),
        "{text}"
    );

    set_activity(
        &pool,
        f.target,
        S::Stopped,
        E::Stopped {
            outcome: "owner_stop".into(),
        },
    )
    .await;
    let text = text_for(&pool, &f, "stopped", armed).await;
    assert!(text.starts_with(&header("stopped")));
    assert!(text.contains("Outcome: owner_stop."), "{text}");

    // Данные другого состояния не подмешиваются: событие устарело, проекция ушла вперёд.
    let text = text_for(&pool, &f, "failed", armed).await;
    assert!(!text.contains("Reason:"), "{text}");
    assert!(!text.contains("owner_stop"), "{text}");

    let mut conn = pool.acquire().await.unwrap();
    let gone = summary::event_text(&mut conn, f.owner, Uuid::now_v7(), "idle", armed)
        .await
        .unwrap();
    assert!(gone.is_none(), "an unknown target has no summary");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_session_without_a_process_does_not_promise_it_will_answer(pool: PgPool) {
    let f = fixture(&pool).await;
    for (state, activity, evidence) in [
        (
            "stopped",
            S::Stopped,
            E::Stopped {
                outcome: "user_stop".into(),
            },
        ),
        (
            "failed",
            S::Failed,
            E::Failed {
                reason: "crashed".into(),
            },
        ),
    ] {
        set_activity(&pool, f.target, activity, evidence).await;
        let text = text_for(&pool, &f, state, None).await;
        assert!(!text.contains("session_send to"), "{text}");
        assert!(
            text.contains("only when the owner resumes it: session_send just queues a message"),
            "{text}"
        );
        assert!(text.contains("session_accept to clean up"), "{text}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lost_session_says_it_may_come_back(pool: PgPool) {
    let f = fixture(&pool).await;
    set_activity(
        &pool,
        f.target,
        S::Failed,
        E::Failed {
            reason: "owner_lost".into(),
        },
    )
    .await;
    let text = text_for(&pool, &f, "failed", None).await;
    assert!(text.contains("Reason: owner_lost."), "{text}");
    assert!(text.contains("you stay subscribed"), "{text}");
    assert!(
        text.ends_with(
            "Next: wait for the next [canterlot] message, or session_accept to give up on it."
        ),
        "{text}"
    );

    set_activity(
        &pool,
        f.target,
        S::Failed,
        E::Failed {
            reason: "crashed".into(),
        },
    )
    .await;
    let text = text_for(&pool, &f, "failed", None).await;
    assert!(!text.contains("you stay subscribed"), "{text}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_never_contains_secret_answer(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let id = open_question(
        &pool,
        &f,
        json!({"questions": [{"id": "q1", "question": "Which token?", "is_secret": true, "answer": "hunter2-in-payload"}]}),
    )
    .await;
    sqlx::query("update requests set decision_payload=$2 where id=$1")
        .bind(id)
        .bind(json!({"answers": {"q1": "hunter2-in-decision"}}))
        .execute(&pool)
        .await
        .unwrap();
    // Закрытый секретный вопрос с решением: его ответ тоже не попадает в текст.
    let closed = open_question(
        &pool,
        &f,
        json!({"questions": [{"id": "q2", "question": "Old secret?", "is_secret": true}]}),
    )
    .await;
    sqlx::query("update requests set state='decision_applied',decision_payload=$2 where id=$1")
        .bind(closed)
        .bind(json!({"answers": {"q2": "hunter2-closed"}}))
        .execute(&pool)
        .await
        .unwrap();
    set_activity(
        &pool,
        f.target,
        S::WaitingForUser,
        E::WaitingForUser {
            phase: "awaiting_answer".into(),
            question: Some("hunter2-in-evidence".into()),
        },
    )
    .await;

    let text = text_for(
        &pool,
        &f,
        "waiting_for_user",
        Some(now - TimeDelta::minutes(1)),
    )
    .await;
    assert!(text.contains("Question: Which token?"), "{text}");
    for secret in [
        "hunter2-in-payload",
        "hunter2-in-decision",
        "hunter2-closed",
        "hunter2-in-evidence",
        "Old secret?",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
}

async fn deliver_event(
    pool: &PgPool,
    owner: Uuid,
    subscriber: Uuid,
    target: Uuid,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
) -> (String, Value) {
    use canterlot_hub::{
        scheduler::{ClaimedJob, JobKind},
        session_tools::jobs,
    };
    let mut tx = pool.begin().await.unwrap();
    let job = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: owner,
        conversation_id: Some(subscriber),
        run_at: Utc::now(),
        payload: json!({
            "action": "event",
            "target_id": target,
            "state": state,
            "chain_depth": 0,
            "armed_at": armed_at,
        }),
        attempts: 0,
    };
    jobs::run_in_tx(&mut tx, &job, Utc::now(), Uuid::now_v7(), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (content, origin): (Value, Option<Value>) = sqlx::query_as(
        "select content,origin from inputs where conversation_id=$1 order by input_seq desc limit 1",
    )
    .bind(subscriber)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        content[0]["text"].as_str().unwrap().to_owned(),
        origin.unwrap_or(Value::Null),
    )
}

async fn expected_summary(
    pool: &PgPool,
    f: &Fixture,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
) -> String {
    let mut conn = pool.acquire().await.unwrap();
    let summary = summary::build(&mut conn, f.owner, f.target, state, armed_at)
        .await
        .unwrap()
        .expect("target exists");
    summary::render(&summary)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_events_deliver_facts_without_changing_text(pool: PgPool) {
    let f = fixture(&pool).await;
    let subscriber = testkit::seed_conversation(&pool, f.owner, f.machine).await;
    let now = Utc::now();
    let armed = Some(now - TimeDelta::minutes(10));
    let answer_id = add_text(&pool, &f, 1, None, None, "All done, tests pass.").await;
    add_run(
        &pool,
        &f,
        Run {
            finished_at: now,
            duration_ms: 252_000,
            git: Some(git(
                "1a2b3c4d5e6f",
                "5d6e7f8a9b0c",
                true,
                &[("5d6e7f8a9b0c", "Parse the header")],
                &[("src/a.rs", "M"), ("src/b.rs", "A")],
            )),
            tokens: (12345, 678),
        },
    )
    .await;

    let expected = expected_summary(&pool, &f, "idle", armed).await;
    let (text, origin) = deliver_event(&pool, f.owner, subscriber, f.target, "idle", armed).await;
    assert_eq!(text, expected, "the delivered text is the rendered summary");
    let event = &origin["event"];
    assert_eq!(origin["kind"], "event", "{origin}");
    assert_eq!(event["kind"], "session_result", "{event}");
    assert_eq!(event["state"], "idle");
    assert_eq!(event["session_id"], json!(f.target));
    assert_eq!(event["turns"], 1);
    assert_eq!(event["duration_ms"], 252_000);
    assert_eq!(event["tokens_in"], 12345);
    assert_eq!(event["tokens_out"], 678);
    assert_eq!(event["commits"][0]["sha"], "5d6e7f8");
    assert_eq!(event["commits"][0]["title"], "Parse the header");
    assert_eq!(event["changed_files"], 2);
    assert_eq!(event["answer"], "All done, tests pass.");
    assert_eq!(event["answer_block_id"], json!(answer_id));
    assert!(event["answer_bytes"].as_i64().unwrap() > 0);

    open_question(
        &pool,
        &f,
        json!({"questions": [{"question": "Which database should I use?"}]}),
    )
    .await;
    set_activity(
        &pool,
        f.target,
        S::WaitingForUser,
        E::WaitingForUser {
            phase: "awaiting_answer".into(),
            question: None,
        },
    )
    .await;
    let expected = expected_summary(&pool, &f, "waiting_for_user", armed).await;
    let (text, origin) = deliver_event(
        &pool,
        f.owner,
        subscriber,
        f.target,
        "waiting_for_user",
        armed,
    )
    .await;
    assert_eq!(text, expected);
    assert_eq!(origin["event"]["kind"], "asked", "{origin}");
    assert_eq!(origin["event"]["state"], "waiting_for_user");
    assert_eq!(origin["event"]["question"], "Which database should I use?");

    set_activity(
        &pool,
        f.target,
        S::Stalled,
        E::Stalled {
            last_event_at: now - TimeDelta::minutes(7),
            phase: "tool".into(),
            process_alive: Some(true),
            cpu_busy: Some(false),
            link: "available".into(),
        },
    )
    .await;
    let expected = expected_summary(&pool, &f, "stalled", armed).await;
    let (text, origin) =
        deliver_event(&pool, f.owner, subscriber, f.target, "stalled", armed).await;
    assert_eq!(text, expected);
    assert_eq!(origin["event"]["kind"], "stalled", "{origin}");
    assert_eq!(origin["event"]["phase"], "tool");
    assert_eq!(origin["event"]["process_alive"], true);
    assert_eq!(origin["event"]["cpu_busy"], false);

    set_activity(
        &pool,
        f.target,
        S::Failed,
        E::Failed {
            reason: "machine_rebooted".into(),
        },
    )
    .await;
    let expected = expected_summary(&pool, &f, "failed", armed).await;
    let (text, origin) = deliver_event(&pool, f.owner, subscriber, f.target, "failed", armed).await;
    assert_eq!(text, expected);
    assert_eq!(origin["event"]["kind"], "failed", "{origin}");
    assert_eq!(origin["event"]["reason"], "machine_rebooted");

    set_activity(
        &pool,
        f.target,
        S::QuotaPaused,
        E::QuotaPaused {
            reason: "limit".into(),
            provider: Some("anthropic".into()),
            window: Some("weekly".into()),
            until: Some(now + TimeDelta::hours(3)),
        },
    )
    .await;
    let expected = expected_summary(&pool, &f, "quota_paused", armed).await;
    let (text, origin) =
        deliver_event(&pool, f.owner, subscriber, f.target, "quota_paused", armed).await;
    assert_eq!(text, expected);
    assert_eq!(origin["event"]["kind"], "quota_paused", "{origin}");
    assert_eq!(origin["event"]["provider"], "anthropic");
    assert_eq!(origin["event"]["window"], "weekly");
    assert!(origin["event"]["until"].is_string());

    set_activity(
        &pool,
        f.target,
        S::Stopped,
        E::Stopped {
            outcome: "owner_stop".into(),
        },
    )
    .await;
    let expected = expected_summary(&pool, &f, "stopped", armed).await;
    let (text, origin) =
        deliver_event(&pool, f.owner, subscriber, f.target, "stopped", armed).await;
    assert_eq!(text, expected);
    assert_eq!(origin["event"]["kind"], "stopped", "{origin}");
    assert_eq!(origin["event"]["outcome"], "owner_stop");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn summary_ignores_answer_from_before_the_window(pool: PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    let old = add_run(
        &pool,
        &f,
        Run {
            finished_at: now - TimeDelta::minutes(20),
            duration_ms: 1000,
            git: None,
            tokens: (1, 1),
        },
    )
    .await;
    add_text(&pool, &f, 1, Some(old), None, "Answer of the previous task").await;
    add_run(
        &pool,
        &f,
        Run {
            finished_at: now + TimeDelta::minutes(5),
            duration_ms: 1000,
            git: None,
            tokens: (1, 1),
        },
    )
    .await;

    let armed = Some(now + TimeDelta::minutes(1));
    let text = text_for(&pool, &f, "idle", armed).await;
    assert!(text.contains("Turns: 1,"), "{text}");
    assert!(text.contains("Final answer: none."), "{text}");
    assert!(!text.contains("previous task"), "{text}");
}
