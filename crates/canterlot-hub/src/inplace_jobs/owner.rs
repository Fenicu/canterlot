//! Задачи владельца без разговора: `canterlot-worker job run` подаёт их по потоку управления
//! (`OwnerJobSubmit`), хаб отвечает `OwnerJobAccepted` или `OwnerJobRefused`. Несущая машина
//! задачи — подавший терминал; домен берётся по пути исходников на его машине. Проверка режима
//! не нужна: это собственный терминал владельца на несущей. Итог уходит подавшему долговечной
//! командой `OwnerJobFinished` при настоящем конце (`carrier::enqueue_owner_finished_in_tx`).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    lock_machine, place,
    spec::{self, NewRunnerJob, Source},
};
use crate::session::Registry;

const MAX_SHELL_BYTES: usize = 64 * 1024;
const MAX_ENV: usize = 32;
const MAX_ENV_VALUE: usize = 4096;
const MAX_TITLE_CHARS: usize = 200;

/// Отказ задаче владельца: код и короткое объяснение уходят терминалу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRefusal {
    pub code: String,
    pub message: String,
}

impl OwnerRefusal {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }
}

impl From<sqlx::Error> for OwnerRefusal {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(
            kind = "owner_job_database",
            error = %crate::dberr::describe(&error),
            "owner job submit failed"
        );
        Self::new("internal", "The hub could not accept the job")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSpec {
    repo: String,
    rev: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerSpec {
    shell: String,
    /// `auto` (по умолчанию) или имя либо идентификатор машины.
    machine: Option<String>,
    title: Option<String>,
    timeout_minutes: Option<i64>,
    lease: Option<String>,
    env: Option<BTreeMap<String, String>>,
    labels: Option<Vec<String>>,
    expected_minutes: Option<i64>,
    placement: Option<Vec<String>>,
    source: SourceSpec,
    outputs: Option<Vec<String>>,
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

/// Подать задачу владельца от несущей `submitter`. Возвращает идентификатор задачи.
pub async fn submit(
    pool: &PgPool,
    registry: &Registry,
    submitter: Uuid,
    spec_json: &str,
) -> Result<Uuid, OwnerRefusal> {
    let spec: OwnerSpec = serde_json::from_str(spec_json)
        .map_err(|_| OwnerRefusal::invalid("the job description is not valid"))?;
    if spec.shell.trim().is_empty()
        || spec.shell.len() > MAX_SHELL_BYTES
        || spec.shell.contains('\0')
    {
        return Err(OwnerRefusal::invalid(
            "shell must be a non-empty command up to 64 KiB without NUL",
        ));
    }
    if spec
        .title
        .as_deref()
        .is_some_and(|title| title.chars().count() > MAX_TITLE_CHARS)
    {
        return Err(OwnerRefusal::invalid("title is longer than 200 characters"));
    }
    if spec
        .timeout_minutes
        .is_some_and(|minutes| !(1..=1440).contains(&minutes))
    {
        return Err(OwnerRefusal::invalid("timeout_minutes must be 1..1440"));
    }
    if spec
        .lease
        .as_deref()
        .is_some_and(|lease| !matches!(lease, "none" | "build" | "gate"))
    {
        return Err(OwnerRefusal::invalid("lease is none, build or gate"));
    }
    let mut env = Vec::new();
    if let Some(pairs) = &spec.env {
        if pairs.len() > MAX_ENV {
            return Err(OwnerRefusal::invalid("at most 32 env pairs"));
        }
        for (name, value) in pairs {
            if !valid_env_name(name) || value.len() > MAX_ENV_VALUE || value.contains('\0') {
                return Err(OwnerRefusal::invalid("invalid env pair"));
            }
            env.push(json!({"name": name, "value": value}));
        }
    }
    let source =
        spec::check_source(&spec.source.repo, &spec.source.rev).map_err(OwnerRefusal::invalid)?;
    let outputs = spec::check_outputs(spec.outputs.as_deref().unwrap_or_default())
        .map_err(OwnerRefusal::invalid)?;
    let labels = spec::check_labels(spec.labels.as_deref().unwrap_or_default())
        .map_err(OwnerRefusal::invalid)?;
    let placement =
        spec::check_placement(spec.placement.as_deref()).map_err(OwnerRefusal::invalid)?;
    let expected_minutes =
        spec::check_expected_minutes(spec.expected_minutes).map_err(OwnerRefusal::invalid)?;
    let id = Uuid::now_v7();
    let registry = registry.clone();
    let shell = spec.shell.clone();
    let title = spec.title.clone();
    let machine = spec.machine.clone();
    let lease = spec.lease.clone().unwrap_or_else(|| "none".to_owned());
    let timeout_seconds = i32::try_from(spec.timeout_minutes.unwrap_or(30) * 60).unwrap_or(1800);
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Только несущая подаёт задачи владельца.
            let row: Option<(Uuid, bool)> = sqlx::query_as(
                "select owner_id, role = 'worker' and is_carrier from machines where id=$1 for share",
            )
            .bind(submitter)
            .fetch_optional(&mut *conn)
            .await?;
            let Some((owner_id, true)) = row else {
                return Ok::<_, sqlx::Error>(Err(OwnerRefusal::new(
                    "not_carrier",
                    "Only a carrier machine may submit owner jobs",
                )));
            };
            let domain = crate::memory::path_domain(conn, submitter, &source.repo)
                .await?
                .map(crate::memory::MemoryDomain::as_str);
            let Some(domain) = domain else {
                return Ok(Err(OwnerRefusal::new(
                    "domain_mismatch",
                    "The memory domain of the source repository is unknown",
                )));
            };
            let target = match machine.as_deref() {
                None | Some("auto") => None,
                Some(name) => match resolve_machine(conn, owner_id, name).await? {
                    Some(target) => Some(target),
                    None => {
                        return Ok(Err(OwnerRefusal::new(
                            "machine_unavailable",
                            "No such machine",
                        )));
                    }
                },
            };
            if let Some(target) = target {
                // Названная машина: сама несущая или её раннер.
                let allowed: bool = sqlx::query_scalar(
                    "select id = $1 or (parent_machine_id = $1 and role = 'runner')
                       from machines where id = $2",
                )
                .bind(submitter)
                .bind(target)
                .fetch_one(&mut *conn)
                .await?;
                if !allowed {
                    return Ok(Err(OwnerRefusal::new(
                        "no_runner_can_satisfy",
                        "The machine is neither this carrier nor one of its runners",
                    )));
                }
                if !place::machine_has_labels(conn, target, &labels).await? {
                    return Ok(Err(OwnerRefusal::new(
                        "no_runner_can_satisfy",
                        "The machine does not carry the labels of the job",
                    )));
                }
                if target == submitter
                    && !place::carrier_workdir_in_domain(conn, submitter, &source.repo, domain)
                        .await?
                {
                    return Ok(Err(OwnerRefusal::new(
                        "domain_mismatch",
                        "The working directory on the carrier would leave the domain of the source",
                    )));
                }
            } else if !place::satisfiable(
                conn,
                submitter,
                &labels,
                &placement,
                &source.repo,
                domain,
            )
            .await?
            {
                return Ok(Err(OwnerRefusal::new(
                    "no_runner_can_satisfy",
                    "No runner can ever satisfy the labels and placement",
                )));
            }
            let new = NewRunnerJob {
                id,
                owner_id,
                conversation_id: None,
                domain: domain.to_owned(),
                carrier_id: submitter,
                shell,
                env: json!(env),
                title,
                timeout_seconds,
                lease,
                labels,
                expected_minutes,
                placement,
                source: Source {
                    repo: source.repo.clone(),
                    rev: source.rev.clone(),
                },
                source_domain: domain.to_owned(),
                outputs,
                owner_job: true,
                call_id: None,
                idempotency_key: None,
                // Итог уходит терминалу командой; сообщения заказчику у задачи нет.
                delivery: Some("message"),
                waiter_instance: None,
                waiter_seconds: 0.0,
            };
            spec::insert_runner_job(conn, &new).await?;
            let correlation = Uuid::now_v7();
            if let Some(target) = target {
                lock_machine(conn, target).await?;
                let job = super::load(conn, id).await?.expect("job row inserted");
                super::emit_changed(conn, &job, correlation).await?;
                place::bind_explicit_in_tx(conn, &job, submitter, target, correlation).await?;
            } else {
                let job = super::load(conn, id).await?.expect("job row inserted");
                super::emit_changed(conn, &job, correlation).await?;
                place::place_waiting_in_tx(conn, &registry, submitter, correlation).await?;
            }
            Ok(Ok(id))
        })
    })
    .await?
}

/// Машина владельца по идентификатору или имени.
async fn resolve_machine(
    conn: &mut sqlx::PgConnection,
    owner_id: Uuid,
    name: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    if let Ok(id) = Uuid::parse_str(name) {
        return sqlx::query_scalar("select id from machines where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(conn)
            .await;
    }
    sqlx::query_scalar("select id from machines where name=$1 and owner_id=$2 order by id limit 1")
        .bind(name)
        .bind(owner_id)
        .fetch_optional(conn)
        .await
}
