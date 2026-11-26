use canterlot_hub::{
    activity::{Evidence, OWNER_LOST},
    executions,
    model::ExecutionState,
    session_tools::summary,
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::cases::run_cases;

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

#[tokio::test]
async fn summary_covers_state_answer_git_usage_duration() {
    let pool = canterlot_hub::testkit::db().await;
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

const WORKTREE: &str = "Worktree: /data/worktrees/x, branch canterlot/0190abcd.\n";

async fn set_worktree(pool: &PgPool, f: &Fixture) {
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(f.target)
        .bind(json!({"source_workdir":"/w","path":"/data/worktrees/x","branch":"canterlot/0190abcd","base":"HEAD","state":"active"}))
        .execute(pool)
        .await
        .unwrap();
}

/// Ход, который начался `minutes` минут назад и не закончился: строка `runs` без конца.
async fn open_run(pool: &PgPool, f: &Fixture, minutes: i64) -> Uuid {
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
        "insert into runs(input_command_id,execution_id,conversation_id,started_at)
         values($1,$2,$3,now()-($4::bigint * interval '1 minute'))",
    )
    .bind(command)
    .bind(f.execution.execution_id)
    .bind(f.target)
    .bind(minutes)
    .execute(pool)
    .await
    .unwrap();
    command
}

/// CLI умер посреди хода: воркер сообщает `crashed`, хаб закрывает открытый ход сам.
async fn crash(pool: &PgPool, f: &Fixture) {
    for (state, outcome) in [
        (ExecutionState::Stopping, None),
        (ExecutionState::Failed, Some("crashed")),
    ] {
        executions::set_state(
            pool,
            f.execution.execution_id,
            state,
            outcome,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }
}

/// Проекция цели в состоянии итога: из неё итог берёт причину и подробности.
async fn derive(pool: &PgPool, f: &Fixture, state: &str, evidence: &Evidence) {
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence)
         values($1,$2,$3)
         on conflict(conversation_id) do update
           set activity_state=excluded.activity_state,activity_evidence=excluded.activity_evidence",
    )
    .bind(f.target)
    .bind(state)
    .bind(serde_json::to_value(evidence).unwrap())
    .execute(pool)
    .await
    .unwrap();
}

/// Время хода считает база в момент сборки: под нагрузкой проходят лишние секунды.
fn whole_minutes(text: &str, minutes: &str) -> String {
    let mut out = text.to_owned();
    for second in 1..10 {
        out = out.replace(&format!("{minutes}m0{second}s"), &format!("{minutes}m00s"));
    }
    out
}

const FAILED_NEXT: &str = "Next: session_inspect for details, or session_accept to clean up. It runs again only when the owner resumes it: session_send just queues a message until then.";

/// Сбой посреди первого хода после часа работы: не «0 ходов за 0 секунд».
async fn crash_mid_first_turn_reports_the_interrupted_turn(pool: PgPool) {
    let f = fixture(&pool).await;
    set_worktree(&pool, &f).await;
    open_run(&pool, &f, 62).await;
    crash(&pool, &f).await;
    derive(
        &pool,
        &f,
        "failed",
        &Evidence::Failed {
            reason: "crashed".into(),
        },
    )
    .await;

    let text = text_for(
        &pool,
        &f,
        "failed",
        Some(Utc::now() - TimeDelta::minutes(70)),
    )
    .await;
    let expected = format!(
        "[canterlot] Session \"Build parser\" (id {}, claude_code, model sonnet) is now failed.\n\
         Reason: crashed.\n\
         Turns: 0 finished, 1 interrupted after 1h02m00s; tokens in/out unknown.\n\
         Git: unknown: the interrupted turn never reported git facts; its work may be in the worktree.\n\
         {WORKTREE}\
         Final answer: none.\n\
         \n\
         {FAILED_NEXT}",
        f.target
    );
    assert_eq!(whole_minutes(&text, "1h02"), expected);
}

/// Зависание посреди хода: ход идёт, его время видно, токенов до конца хода нет.
async fn stall_mid_turn_reports_the_running_turn(pool: PgPool) {
    let f = fixture(&pool).await;
    set_worktree(&pool, &f).await;
    open_run(&pool, &f, 31).await;
    let last_event_at = DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .unwrap()
        .to_utc();
    derive(
        &pool,
        &f,
        "stalled",
        &Evidence::Stalled {
            last_event_at,
            phase: "generating".into(),
            process_alive: Some(true),
            cpu_busy: Some(false),
            link: "available".into(),
            hung: None,
        },
    )
    .await;

    let text = text_for(
        &pool,
        &f,
        "stalled",
        Some(Utc::now() - TimeDelta::minutes(40)),
    )
    .await;
    let expected = format!(
        "[canterlot] Session \"Build parser\" (id {}, claude_code, model sonnet) is now stalled.\n\
         Phase: generating. Last event: 2026-01-02T03:04:05Z.\n\
         Process alive: yes. CPU busy: no.\n\
         Turns: 0 finished, 1 running for 31m00s; tokens in/out unknown.\n\
         Git: unknown until the running turn ends; its work may be in the worktree.\n\
         {WORKTREE}\
         \n\
         Next: session_inspect to look closer, or session_send to nudge it.",
        f.target
    );
    assert_eq!(whole_minutes(&text, "31"), expected);
}

/// Потерянный владелец посреди хода: открытый ход остаётся в итоге, а не пропадает.
async fn owner_lost_mid_turn_reports_the_lost_turn(pool: PgPool) {
    let f = fixture(&pool).await;
    set_worktree(&pool, &f).await;
    open_run(&pool, &f, 45).await;
    executions::set_state(
        &pool,
        f.execution.execution_id,
        ExecutionState::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    derive(
        &pool,
        &f,
        "failed",
        &Evidence::Failed {
            reason: OWNER_LOST.into(),
        },
    )
    .await;

    let text = text_for(
        &pool,
        &f,
        "failed",
        Some(Utc::now() - TimeDelta::minutes(50)),
    )
    .await;
    let expected = format!(
        "[canterlot] Session \"Build parser\" (id {}, claude_code, model sonnet) is now failed.\n\
         Reason: owner_lost. The worker lost this session but may get it back; you stay subscribed.\n\
         Turns: 0 finished, 1 lost after 45m00s; tokens in/out unknown.\n\
         Git: unknown: the lost turn never reported git facts; its work may be in the worktree.\n\
         {WORKTREE}\
         Final answer: none.\n\
         \n\
         Next: wait for the next [canterlot] message, or session_accept to give up on it.",
        f.target
    );
    assert_eq!(whole_minutes(&text, "45"), expected);
}

/// Законченный ход и оборванный следующий: законченный считается как раньше, оборванный
/// назван отдельно, а git — факты законченного хода с оговоркой.
async fn crash_after_a_finished_turn_keeps_its_facts(pool: PgPool) {
    let f = fixture(&pool).await;
    set_worktree(&pool, &f).await;
    let finished = add_run(
        &pool,
        &f,
        Run {
            finished_at: Utc::now() - TimeDelta::minutes(13),
            duration_ms: 240_000,
            git: Some(git(
                "1a2b3c4d5e6f",
                "5d6e7f8a9b0c",
                true,
                &[("5d6e7f8a9b0c", "Parse the header")],
                &[("src/a.rs", "M")],
            )),
            tokens: (12345, 678),
        },
    )
    .await;
    add_text(
        &pool,
        &f,
        1,
        Some(finished),
        None,
        "Header parsed, starting on the body.",
    )
    .await;
    open_run(&pool, &f, 12).await;
    crash(&pool, &f).await;
    derive(
        &pool,
        &f,
        "failed",
        &Evidence::Failed {
            reason: "crashed".into(),
        },
    )
    .await;

    let text = text_for(
        &pool,
        &f,
        "failed",
        Some(Utc::now() - TimeDelta::minutes(20)),
    )
    .await;
    let expected = format!(
        "[canterlot] Session \"Build parser\" (id {}, claude_code, model sonnet) is now failed.\n\
         Reason: crashed.\n\
         Turns: 1 finished, 1 interrupted after 12m00s; duration 16m00s, tokens in/out 12345/678 in finished turns.\n\
         HEAD 1a2b3c4 -> 5d6e7f8, 1 commit:\n\
         - 5d6e7f8 Parse the header\n\
         Changed files (1):\n\
         M src/a.rs\n\
         Uncommitted changes: yes\n\
         Git facts are from the last finished turn; the interrupted turn may have changed more.\n\
         {WORKTREE}\
         Final answer:\n\
         Header parsed, starting on the body.\n\
         \n\
         {FAILED_NEXT}",
        f.target
    );
    assert_eq!(whole_minutes(&whole_minutes(&text, "12"), "16"), expected);
}

/// Итог сессии, которая упала, зависла или потерялась посреди хода: каждый случай
/// заводит свою цель на общей базе.
#[tokio::test]
async fn unfinished_turn_cases() {
    let pool = canterlot_hub::testkit::db().await;
    run_cases!(pool.clone();
        crash_mid_first_turn_reports_the_interrupted_turn,
        stall_mid_turn_reports_the_running_turn,
        owner_lost_mid_turn_reports_the_lost_turn,
        crash_after_a_finished_turn_keeps_its_facts,
    );
}
