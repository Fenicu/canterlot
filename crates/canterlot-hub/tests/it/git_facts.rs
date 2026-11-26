use crate::support::task3 as support;
use canterlot_proto::worker::v1::{record_body::Body, *};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn facts(before: &str, after: &str, commits: &[(&str, &str)], files: &[(&str, &str)]) -> GitFacts {
    GitFacts {
        available: true,
        head_before: before.into(),
        head_after: after.into(),
        dirty: true,
        truncated: false,
        commits: commits
            .iter()
            .map(|(sha, subject)| GitCommit {
                sha: (*sha).into(),
                subject: (*subject).into(),
                committed_at_ms: 0,
            })
            .collect(),
        files: files
            .iter()
            .map(|(path, status)| GitFile {
                path: (*path).into(),
                status: (*status).into(),
            })
            .collect(),
    }
}

async fn stored(pool: &PgPool) -> Option<Value> {
    sqlx::query_scalar("select git from runs")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn with_git(body: Body, git: Option<GitFacts>, adopted: bool) -> Body {
    let Body::RunFinished(mut f) = body else {
        unreachable!()
    };
    f.git = git;
    f.adopted = adopted;
    Body::RunFinished(f)
}

#[tokio::test]
async fn ingest_stores_git_facts() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![accepted(i.command_id, ms)]).await;
    let git = facts(
        "aaa",
        "bbb",
        &[("bbb", "Add feature")],
        &[("src/lib.rs", "M")],
    );
    r.send(vec![with_git(
        finished(&r, i.command_id, ms + 1000),
        Some(git),
        false,
    )])
    .await;
    assert_eq!(
        stored(&pool).await,
        Some(json!({
            "available": true,
            "head_before": "aaa",
            "head_after": "bbb",
            "dirty": true,
            "truncated": false,
            "commits": [{"sha": "bbb", "subject": "Add feature"}],
            "files": [{"path": "src/lib.rs", "status": "M"}],
        }))
    );
    Ok(())
}

fn terminal(r: &Run, state: &str, outcome: &str, git: Option<GitFacts>) -> Body {
    Body::ExecutionState(ExecutionState {
        execution_id: r.execution.execution_id.to_string(),
        generation: r.execution.generation as u64,
        state: state.into(),
        outcome: outcome.into(),
        interrupted_git: git,
        ..Default::default()
    })
}

async fn closed_run(pool: &PgPool, r: &Run) -> (bool, Option<Value>) {
    sqlx::query_as("select forced_close, git from runs where execution_id=$1")
        .bind(r.execution.execution_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// CLI умер посреди хода: факты git приходят с конечным состоянием и ложатся на ход,
/// который хаб закрыл сам; ход остаётся прерванным. Старый воркер фактов не шлёт, и ход
/// закрывается без них, как раньше.
#[tokio::test]
async fn terminal_state_stores_git_facts_of_the_interrupted_turn() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for git in [
        Some(facts(
            "aaa",
            "bbb",
            &[("bbb", "Half done")],
            &[("src/lib.rs", "M")],
        )),
        None,
    ] {
        let mut r = Run::new(&pool).await;
        let i = r.input(&pool).await;
        r.send(vec![accepted(i.command_id, Utc::now().timestamp_millis())])
            .await;
        r.send(vec![
            terminal(&r, "stopping", "", None),
            terminal(&r, "failed", "crashed", git.clone()),
        ])
        .await;
        let (forced, stored) = closed_run(&pool, &r).await;
        assert!(forced, "the hub closed the turn itself");
        match git {
            Some(_) => assert_eq!(
                stored,
                Some(json!({
                    "available": true,
                    "head_before": "aaa",
                    "head_after": "bbb",
                    "dirty": true,
                    "truncated": false,
                    "commits": [{"sha": "bbb", "subject": "Half done"}],
                    "files": [{"path": "src/lib.rs", "status": "M"}],
                }))
            ),
            None => assert_eq!(stored, None),
        }
    }
    Ok(())
}

/// Ход закончен обычным RunFinished: факты конечного состояния его не трогают.
#[tokio::test]
async fn terminal_git_facts_leave_a_finished_turn_alone() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![accepted(i.command_id, ms)]).await;
    r.send(vec![with_git(
        finished(&r, i.command_id, ms + 1000),
        Some(facts("aaa", "bbb", &[], &[])),
        false,
    )])
    .await;
    r.send(vec![
        terminal(&r, "stopping", "", None),
        terminal(
            &r,
            "failed",
            "crashed",
            Some(facts("bbb", "ccc", &[("ccc", "Stray")], &[])),
        ),
    ])
    .await;
    let (forced, stored) = closed_run(&pool, &r).await;
    assert!(!forced);
    assert_eq!(stored.unwrap()["head_after"], "bbb");
    Ok(())
}
