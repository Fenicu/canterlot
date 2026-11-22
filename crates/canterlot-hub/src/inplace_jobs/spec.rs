//! Описание задачи на раннере: проверка полей `job_run` (общая для инструмента агента и для
//! задачи владельца из терминала) и вставка строки с исходниками.

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::place::{PLACEMENT_CLOUD, PLACEMENT_LOCAL, PLACEMENT_RUNNER};

pub const MAX_OUTPUTS: usize = 32;
pub const MAX_LABELS: usize = 16;
pub const MAX_LABEL_BYTES: usize = 64;
pub const MAX_PATH_BYTES: usize = 4096;
/// Предел пути результата, как у несущей (`collect::valid_relative`).
pub const MAX_OUTPUT_PATH_BYTES: usize = 512;
pub const DEFAULT_EXPECTED_MINUTES: i64 = 30;

/// Исходники: абсолютный путь репозитория на машине заказчика и полный sha коммита.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub repo: String,
    pub rev: String,
}

fn has_parent_component(path: &str) -> bool {
    path.split('/').any(|part| part == "..")
}

/// Путь репозитория без хвостового `/`; ревизия — 40 шестнадцатеричных знаков в нижнем регистре.
pub fn check_source(repo: &str, rev: &str) -> Result<Source, &'static str> {
    if !repo.starts_with('/')
        || repo.len() > MAX_PATH_BYTES
        || repo.contains('\0')
        || repo.contains('\n')
        || has_parent_component(repo)
    {
        return Err("source.repo must be an absolute path without '..'");
    }
    let repo = repo.trim_end_matches('/');
    if repo.is_empty() {
        return Err("source.repo must name a repository directory");
    }
    if rev.len() != 40 || !rev.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("source.rev must be a full 40-hex commit sha");
    }
    Ok(Source {
        repo: repo.to_owned(),
        rev: rev.to_ascii_lowercase(),
    })
}

/// Путь результата в том виде, который примет несущая при сборе: относительный, из знаков
/// `[A-Za-z0-9._/+@-]`, без пустых частей, `.` и `..`, не с `-`, до 512 байт. Хаб отказывает
/// заранее, чтобы задача не выполнялась и не падала на имени после работы.
fn valid_output_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_OUTPUT_PATH_BYTES
        && !path.starts_with('/')
        && !path.starts_with('-')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b'@')
        })
}

/// Относительные пути результатов: не больше 32, из безопасных знаков, без `..`, без абсолютных.
pub fn check_outputs(outputs: &[String]) -> Result<Vec<String>, &'static str> {
    if outputs.len() > MAX_OUTPUTS {
        return Err("at most 32 outputs");
    }
    let mut out = Vec::with_capacity(outputs.len());
    for path in outputs {
        let trimmed = path.trim_end_matches('/');
        if !valid_output_path(trimmed) {
            return Err(
                "outputs must be relative paths of [A-Za-z0-9._/+@-] without '..', up to 512 bytes",
            );
        }
        if !out.iter().any(|seen| seen == trimmed) {
            out.push(trimmed.to_owned());
        }
    }
    Ok(out)
}

pub fn check_labels(labels: &[String]) -> Result<Vec<String>, &'static str> {
    if labels.len() > MAX_LABELS {
        return Err("at most 16 labels");
    }
    let mut out: Vec<String> = Vec::with_capacity(labels.len());
    for label in labels {
        if label.is_empty()
            || label.len() > MAX_LABEL_BYTES
            || !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err("a label is 1..64 characters of letters, digits, '.', '_' or '-'");
        }
        if !out.contains(label) {
            out.push(label.clone());
        }
    }
    Ok(out)
}

/// Виды места; пусто — все. Повтор схлопывается.
pub fn check_placement(placement: Option<&[String]>) -> Result<Vec<String>, &'static str> {
    let Some(placement) = placement else {
        return Ok(vec![
            PLACEMENT_LOCAL.to_owned(),
            PLACEMENT_RUNNER.to_owned(),
            PLACEMENT_CLOUD.to_owned(),
        ]);
    };
    if placement.is_empty() {
        return Err("placement must name at least one of local, runner, cloud");
    }
    let mut out: Vec<String> = Vec::new();
    for kind in placement {
        if ![PLACEMENT_LOCAL, PLACEMENT_RUNNER, PLACEMENT_CLOUD].contains(&kind.as_str()) {
            return Err("placement is local, runner or cloud");
        }
        if !out.contains(kind) {
            out.push(kind.clone());
        }
    }
    Ok(out)
}

pub fn check_expected_minutes(minutes: Option<i64>) -> Result<i32, &'static str> {
    let minutes = minutes.unwrap_or(DEFAULT_EXPECTED_MINUTES);
    i32::try_from(minutes)
        .ok()
        .filter(|minutes| (1..=1440).contains(minutes))
        .ok_or("expected_minutes must be 1..1440")
}

/// Строка задачи, подаваемой хабу с исходниками. Машины и каталога нет: выбор делает
/// размещение, а названную машину привязывает вызывающий после вставки.
pub struct NewRunnerJob {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub domain: String,
    pub carrier_id: Uuid,
    pub shell: String,
    pub env: Value,
    pub title: Option<String>,
    pub timeout_seconds: i32,
    pub lease: String,
    pub labels: Vec<String>,
    pub expected_minutes: i32,
    pub placement: Vec<String>,
    pub source: Source,
    pub source_domain: String,
    pub outputs: Vec<String>,
    pub owner_job: bool,
    pub call_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub delivery: Option<&'static str>,
    pub waiter_instance: Option<Uuid>,
    pub waiter_seconds: f64,
}

/// Вставка; `false` — гонка повторов с тем же ключом, строки нет.
pub async fn insert_runner_job(
    conn: &mut PgConnection,
    job: &NewRunnerJob,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "insert into jobs (id, owner_id, conversation_id, domain, machine_id, remote, workdir,
                           shell, env, title, timeout_seconds, lease, call_id, idempotency_key,
                           delivery, waiter_instance, waiter_deadline, state, labels,
                           expected_minutes, placement, source_repo, source_rev, source_domain,
                           outputs, carrier_id, owner_job)
         values ($1,$2,$3,$4,null,false,null,$5,$6,$7,$8,$9,$10,$11,$12,$13,
                 case when $13::uuid is null then null
                      else clock_timestamp() + make_interval(secs => $14) end,
                 'waiting_runner',$15,$16,$17,$18,$19,$20,$21,$22,$23)
         on conflict do nothing",
    )
    .bind(job.id)
    .bind(job.owner_id)
    .bind(job.conversation_id)
    .bind(&job.domain)
    .bind(&job.shell)
    .bind(&job.env)
    .bind(job.title.as_deref())
    .bind(job.timeout_seconds)
    .bind(&job.lease)
    .bind(job.call_id)
    .bind(job.idempotency_key.as_deref())
    .bind(job.delivery)
    .bind(job.waiter_instance)
    .bind(job.waiter_seconds)
    .bind(&job.labels)
    .bind(job.expected_minutes)
    .bind(&job.placement)
    .bind(&job.source.repo)
    .bind(&job.source.rev)
    .bind(&job.source_domain)
    .bind(&job.outputs)
    .bind(job.carrier_id)
    .bind(job.owner_job)
    .execute(conn)
    .await?
    .rows_affected()
        > 0)
}
