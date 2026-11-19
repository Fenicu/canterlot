//! Разовый импорт памяти Claude: применение пакета (`memory::import::apply`).

use canterlot_hub::{
    config::MemoryConfig,
    memory::import::{Bundle, BundleEntry, ImportReport, apply},
    testkit,
};
use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;

const WORK: &str = "/h/work/thing";

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, day, 10, 0, 0).unwrap()
}

fn entry(subject: &str, body: &str, source: &str, sha: &str) -> BundleEntry {
    BundleEntry {
        domain: "work".into(),
        project: Some(WORK.into()),
        kind: "gotcha".into(),
        subject: subject.into(),
        body: body.into(),
        source_path: source.into(),
        sha256: sha.into(),
        updated_at: at(5),
    }
}

fn bundle(entries: Vec<BundleEntry>) -> Bundle {
    Bundle {
        bundle_id: "b-1".into(),
        created_at: at(6),
        entries,
    }
}

async fn count(pool: &PgPool, table: &'static str) -> i64 {
    let sql: &'static str = match table {
        "memory_entries" => "select count(*) from memory_entries",
        "memory_events" => "select count(*) from memory_events",
        other => panic!("unknown table {other}"),
    };
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn apply_dry_run_writes_nothing(pool: PgPool) {
    testkit::seed(&pool).await;
    let config = MemoryConfig::default();
    let bundle = bundle(vec![
        entry("Первая", "тело один", "-h-work-thing/memory/a.md", "aa"),
        entry("Вторая", "тело два", "-h-work-thing/memory/b.md", "bb"),
    ]);

    let dry = apply(&pool, &bundle, true, &config).await.unwrap();

    assert_eq!(dry.created, 2);
    assert_eq!(count(&pool, "memory_entries").await, 0);
    assert_eq!(count(&pool, "memory_events").await, 0);
    let real = apply(&pool, &bundle, false, &config).await.unwrap();
    assert_eq!(real, dry, "сухой прогон даёт тот же отчёт");
    assert_eq!(count(&pool, "memory_entries").await, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn apply_is_idempotent(pool: PgPool) {
    testkit::seed(&pool).await;
    let config = MemoryConfig::default();
    let bundle = bundle(vec![entry("Первая", "тело", "-h-work-thing/memory/a.md", "aa")]);

    let first = apply(&pool, &bundle, false, &config).await.unwrap();
    let second = apply(&pool, &bundle, false, &config).await.unwrap();

    assert_eq!(first.created, 1);
    assert_eq!(
        second,
        ImportReport {
            skipped_existing: 1,
            ..Default::default()
        }
    );
    assert_eq!(count(&pool, "memory_entries").await, 1);
    // Другое содержимое того же файла получает новую запись: ключ — путь и хэш.
    let changed = self::bundle(vec![entry(
        "Первая",
        "тело изменено",
        "-h-work-thing/memory/a.md",
        "ab",
    )]);
    let third = apply(&pool, &changed, false, &config).await.unwrap();
    assert_eq!(third.created, 1);
    assert_eq!(third.renamed_collisions, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn apply_renames_subject_collision_with_suffix_2(pool: PgPool) {
    testkit::seed(&pool).await;
    let config = MemoryConfig::default();
    let bundle = bundle(vec![
        entry("Один и тот же", "тело 1", "-h-work-thing/memory/a.md", "a1"),
        entry("один и тот же", "тело 2", "-h-work-thing/memory/b.md", "b1"),
        entry("Один и тот же", "тело 3", "-h-work-thing/memory/c.md", "c1"),
    ]);

    let report = apply(&pool, &bundle, false, &config).await.unwrap();

    assert_eq!(report.created, 3);
    assert_eq!(report.renamed_collisions, 2);
    let subjects: Vec<String> =
        sqlx::query_scalar("select subject from memory_entries order by created_at, subject")
            .fetch_all(&pool)
            .await
            .unwrap();
    for wanted in ["Один и тот же", "один и тот же (2)", "Один и тот же (3)"] {
        assert!(
            subjects.iter().any(|subject| subject == wanted),
            "{wanted} в {subjects:?}"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn apply_refuses_secret_entry_even_if_bundle_has_it(pool: PgPool) {
    testkit::seed(&pool).await;
    let config = MemoryConfig::default();
    let token = format!("ghp_{}", "x".repeat(36));
    let bundle = bundle(vec![
        entry(
            "Невинное имя",
            &format!("ключ {token} лежит в теле"),
            "-h-work-thing/memory/notes.md",
            "n1",
        ),
        entry("Чистая", "ничего секретного", "-h-work-thing/memory/ok.md", "o1"),
    ]);

    let report = apply(&pool, &bundle, false, &config).await.unwrap();

    assert_eq!(report.refused_secrets, 1);
    assert_eq!(report.created, 1);
    let bodies: Vec<String> = sqlx::query_scalar("select body from memory_entries")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(bodies.iter().all(|body| !body.contains("ghp_")), "{bodies:?}");
}

#[sqlx::test(migrations = "./migrations")]
async fn frontmatter_description_to_subject_type_to_kind_modified_to_updated_at(pool: PgPool) {
    testkit::seed(&pool).await;
    let config = MemoryConfig::default();
    let long_subject = format!("{} конец", "слово ".repeat(40));
    let mut odd_kind = entry(
        "Строка\nдве   строки",
        "тело",
        "-h-work-thing/memory/odd.md",
        "o1",
    );
    odd_kind.kind = "feedback_notes".into();
    odd_kind.updated_at = at(2);
    let mut long = entry(&long_subject, "тело", "-h-work-thing/memory/long.md", "l1");
    long.kind = "decision".into();
    long.updated_at = at(3);
    let bundle = bundle(vec![odd_kind, long]);

    let report = apply(&pool, &bundle, false, &config).await.unwrap();

    assert_eq!(report.created, 2);
    assert_eq!(report.kind_remapped, 1, "вид вне восьми даёт reference");
    let rows: Vec<(String, String, DateTime<Utc>, serde_json::Value)> = sqlx::query_as(
        "select subject, kind, updated_at, import_source from memory_entries order by updated_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let (subject, kind, updated, source) = &rows[0];
    assert_eq!(subject, "Строка две строки", "одна строка");
    assert_eq!(kind, "reference");
    assert_eq!(*updated, at(2), "время изменения файла становится updated_at");
    assert_eq!(source["path"], "-h-work-thing/memory/odd.md");
    assert_eq!(source["sha256"], "o1");
    let (subject, kind, updated, _) = &rows[1];
    assert!(subject.chars().count() <= 160);
    assert!(subject.ends_with("слово"), "обрезка по границе слова: {subject}");
    assert_eq!(kind, "decision");
    assert_eq!(*updated, at(3));
}
