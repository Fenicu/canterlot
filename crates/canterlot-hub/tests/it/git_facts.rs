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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ingest_stores_git_facts(pool: PgPool) -> TestResult {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn old_worker_run_finished_keeps_null_git(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![accepted(i.command_id, ms)]).await;
    r.send(vec![finished(&r, i.command_id, ms + 1000)]).await;
    assert_eq!(stored(&pool).await, None);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn adopted_run_merges_git_facts(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![accepted(i.command_id, ms)]).await;
    let first = facts(
        "aaa",
        "bbb",
        &[("bbb", "first")],
        &[("a.rs", "A"), ("b.rs", "M")],
    );
    r.send(vec![with_git(
        finished(&r, i.command_id, ms + 1000),
        Some(first),
        false,
    )])
    .await;
    let later = facts(
        "bbb",
        "ccc",
        &[("ccc", "second")],
        &[("b.rs", "D"), ("c.rs", "A")],
    );
    r.send(vec![with_git(
        finished(&r, i.command_id, ms + 2000),
        Some(later),
        true,
    )])
    .await;
    let git = stored(&pool).await.expect("git");
    assert_eq!(git["head_before"], "aaa");
    assert_eq!(git["head_after"], "ccc");
    assert_eq!(
        git["commits"],
        json!([{"sha": "bbb", "subject": "first"}, {"sha": "ccc", "subject": "second"}])
    );
    assert_eq!(
        git["files"],
        json!([
            {"path": "a.rs", "status": "A"},
            {"path": "b.rs", "status": "D"},
            {"path": "c.rs", "status": "A"},
        ])
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn adopted_run_without_stored_facts_sets_them(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![accepted(i.command_id, ms)]).await;
    r.send(vec![finished(&r, i.command_id, ms + 1000)]).await;
    let later = facts("bbb", "ccc", &[("ccc", "second")], &[]);
    r.send(vec![with_git(
        finished(&r, i.command_id, ms + 2000),
        Some(later),
        true,
    )])
    .await;
    let git = stored(&pool).await.expect("git");
    assert_eq!(git["head_after"], "ccc");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_bounds_oversized_git_facts(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![accepted(i.command_id, ms)]).await;
    let commits: Vec<_> = (0..80)
        .map(|n| (format!("sha{n}"), "я".repeat(400)))
        .collect();
    let refs: Vec<_> = commits
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let files: Vec<_> = (0..300).map(|n| (format!("f{n}.rs\u{0}x"), "M")).collect();
    let file_refs: Vec<_> = files.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    let git = facts("aaa", "bbb", &refs, &file_refs);
    r.send(vec![with_git(
        finished(&r, i.command_id, ms + 1000),
        Some(git),
        false,
    )])
    .await;
    let git = stored(&pool).await.expect("git");
    assert_eq!(git["truncated"], true);
    assert_eq!(git["commits"].as_array().unwrap().len(), 50);
    assert_eq!(git["files"].as_array().unwrap().len(), 200);
    let subject = git["commits"][0]["subject"].as_str().unwrap();
    assert!(subject.len() <= 256);
    let path = git["files"][0]["path"].as_str().unwrap();
    assert!(!path.contains('\u{0}'));
    Ok(())
}
