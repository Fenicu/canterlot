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
