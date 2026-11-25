use canterlot_hub::{executions, model::ExecutionState, session_tools::summary, testkit};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
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
