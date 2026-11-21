//! Размещение задач с исходниками: хаб сам выбирает машину из раннеров несущей и самой
//! несущей. Задача без подходящего места остаётся в `waiting_runner` без машины, а
//! `place_waiting_in_tx` вызывается при подаче, при освобождении места, при подключении
//! раннера, при включении выключателя и таймером как страховка.
//!
//! Порядок блокировок: замки машин несущей и её раннеров (по возрастанию идентификатора),
//! затем строка задачи, затем счётчик событий. Замки машин берутся все сразу и блокирующе:
//! две параллельные расстановки одной несущей идут в одном порядке и не замыкают цикл, а
//! привязка в одной из них видна другой через `occupied_in_tx`.

use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    Job, State, emit_changed, lock_machine, lock_row, max_running_in_tx, notify_outbox,
    occupied_in_tx, repo_dir_name,
};
use crate::session::{Availability, Registry};

/// Виды места в `placement` задачи.
pub const PLACEMENT_LOCAL: &str = "local";
pub const PLACEMENT_RUNNER: &str = "runner";
pub const PLACEMENT_CLOUD: &str = "cloud";

/// Кандидат на размещение: сама несущая (ранг 0) или её раннер.
#[derive(sqlx::FromRow, Debug, Clone)]
struct Candidate {
    id: Uuid,
    rank: i32,
    labels: Vec<String>,
    root_dir: Option<String>,
    accepts_remote_work: bool,
    capabilities: Vec<String>,
}

impl Candidate {
    fn is_carrier(&self, carrier: Uuid) -> bool {
        self.id == carrier
    }

    /// Ранг размещения: несущая всегда 0, раннер — из своей строки.
    fn placement_rank(&self, carrier: Uuid) -> i32 {
        if self.is_carrier(carrier) {
            0
        } else {
            self.rank
        }
    }

    fn capable(&self) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == crate::session::CAPABILITY_JOBS_INPLACE_V1)
    }

    fn has_labels(&self, wanted: &[String]) -> bool {
        wanted.iter().all(|label| self.labels.contains(label))
    }

    fn allowed_by(&self, carrier: Uuid, placement: &[String]) -> bool {
        let kind = if self.is_carrier(carrier) {
            PLACEMENT_LOCAL
        } else {
            PLACEMENT_RUNNER
        };
        placement.iter().any(|allowed| allowed == kind)
    }
}

async fn candidates(conn: &mut PgConnection, carrier: Uuid) -> Result<Vec<Candidate>, sqlx::Error> {
    sqlx::query_as(
        "select m.id, m.rank, m.labels, m.root_dir, m.accepts_remote_work, m.capabilities
         from machines m
         where m.id = $1
            or (m.parent_machine_id = $1 and m.role = 'runner'
                and exists(select 1 from machine_credentials c
                            where c.machine_id = m.id and c.revoked_at is null))
         order by m.id",
    )
    .bind(carrier)
    .fetch_all(conn)
    .await
}

/// Задача, которую можно когда-нибудь разместить: есть настроенное место с её метками среди
/// разрешённых видов. Включённость, связь и свободные места не считаются: это отказ при
/// подаче, а не ожидание. Облако (`cloud`) пока места не даёт: оно относится к этапу C.
pub async fn satisfiable(
    conn: &mut PgConnection,
    carrier: Uuid,
    labels: &[String],
    placement: &[String],
) -> Result<bool, sqlx::Error> {
    Ok(candidates(conn, carrier).await?.iter().any(|candidate| {
        candidate.capable()
            && candidate.allowed_by(carrier, placement)
            && candidate.has_labels(labels)
    }))
}

/// Ключ голого репозитория: имя каталога и 12 первых шестнадцатеричных знаков sha256 от
/// абсолютного пути. Одинаковые имена в разных местах не делят хранилище объектов.
pub fn repo_key(repo: &str) -> String {
    let digest = Sha256::digest(repo.as_bytes());
    let hash: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}-{hash}", repo_dir_name(repo))
}

/// Рабочий каталог задачи: на раннере `<root>/jobs/<домен>/<id>`, на самой несущей
/// `<каталог над репозиторием>/.canterlot-jobs/<имя репозитория>/<id>`. Второй лежит рядом с
/// исходным репозиторием, поэтому домен по пути у него тот же.
pub fn workdir_for(job: &Job, root_dir: Option<&str>, on_carrier: bool) -> Option<String> {
    let repo = job.source_repo.as_deref()?;
    let domain = job.source_domain.as_deref()?;
    if on_carrier {
        let trimmed = repo.trim_end_matches('/');
        let parent = trimmed.rsplit_once('/').map_or("", |(parent, _)| parent);
        Some(format!(
            "{parent}/.canterlot-jobs/{}/{}",
            repo_dir_name(repo),
            job.id
        ))
    } else {
        Some(format!(
            "{}/jobs/{domain}/{}",
            root_dir?.trim_end_matches('/'),
            job.id
        ))
    }
}

/// Привязка: машина, рабочий каталог, команда подготовки несущей. Под замками машины и строки.
async fn bind_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    carrier: Uuid,
    target: &Candidate,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    bind_target_in_tx(
        conn,
        job,
        carrier,
        target.id,
        target.root_dir.as_deref(),
        correlation_id,
    )
    .await
}

/// Привязка к названной машине без выбора (`machine` назван в заявке): место не проверяется,
/// задача встанет в очередь машины после подготовки, как любая удалённая. Под замком машины.
pub async fn bind_explicit_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    carrier: Uuid,
    target: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let root: Option<String> = sqlx::query_scalar("select root_dir from machines where id=$1")
        .bind(target)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    bind_target_in_tx(conn, job, carrier, target, root.as_deref(), correlation_id).await
}

async fn bind_target_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    carrier: Uuid,
    target: Uuid,
    root_dir: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let on_carrier = target == carrier;
    let Some(workdir) = workdir_for(job, root_dir, on_carrier) else {
        // Раннер без корня не бывает (проверка строки), задача без исходников сюда не идёт.
        return Ok(());
    };
    let generation: Option<i64> = if on_carrier {
        None
    } else {
        sqlx::query_scalar("select remote_admission_generation from machines where id=$1")
            .bind(target)
            .fetch_optional(&mut *conn)
            .await?
    };
    sqlx::query(
        "update jobs set machine_id=$2, workdir=$3, remote=$4, admission_generation=$5,
                state='staging', last_fact_at=clock_timestamp(), updated_at=clock_timestamp()
         where id=$1",
    )
    .bind(job.id)
    .bind(target)
    .bind(&workdir)
    .bind(!on_carrier)
    .bind(generation)
    .execute(&mut *conn)
    .await?;
    let moved = super::load(conn, job.id)
        .await?
        .expect("job row just updated");
    super::carrier::send_stage_in_tx(conn, &moved, root_dir.filter(|_| !on_carrier)).await?;
    emit_changed(conn, &moved, correlation_id).await?;
    notify_outbox(conn, carrier).await?;
    Ok(())
}

/// Расставляет ожидающие задачи одной несущей по порядку подачи. Возвращает, сколько привязано.
/// Свободное место — `occupied_in_tx < jobs_max_running`, кандидат на связи по реестру;
/// побеждает больший ранг, при равенстве меньший идентификатор машины.
pub async fn place_waiting_in_tx(
    conn: &mut PgConnection,
    registry: &Registry,
    carrier: Uuid,
    correlation_id: Uuid,
) -> Result<usize, sqlx::Error> {
    let waiting: Vec<Uuid> = sqlx::query_scalar(
        "select id from jobs where carrier_id=$1 and state='waiting_runner'
         order by created_at, id",
    )
    .bind(carrier)
    .fetch_all(&mut *conn)
    .await?;
    if waiting.is_empty() {
        return Ok(0);
    }
    let mut all = candidates(conn, carrier).await?;
    // Замки всех машин сразу и по возрастанию: параллельные расстановки не замыкают цикл.
    for candidate in &all {
        lock_machine(conn, candidate.id).await?;
    }
    all.sort_by(|a, b| {
        b.placement_rank(carrier)
            .cmp(&a.placement_rank(carrier))
            .then(a.id.cmp(&b.id))
    });
    let mut placed = 0;
    for id in waiting {
        let Some(job) = lock_row(conn, id).await? else {
            continue;
        };
        if job.state() != State::WaitingRunner {
            continue;
        }
        let mut chosen = None;
        for candidate in &all {
            if !candidate.capable()
                || !candidate.allowed_by(carrier, &job.placement)
                || !candidate.has_labels(&job.labels)
                || registry.availability(candidate.id) != Availability::Available
            {
                continue;
            }
            // Пауза раннера: выключатель закрывает новые привязки. Несущая берёт свою работу.
            if !candidate.is_carrier(carrier) && !candidate.accepts_remote_work {
                continue;
            }
            if occupied_in_tx(conn, candidate.id).await?
                >= max_running_in_tx(conn, candidate.id).await?
            {
                continue;
            }
            chosen = Some(candidate);
            break;
        }
        if let Some(target) = chosen {
            bind_in_tx(conn, &job, carrier, target, correlation_id).await?;
            placed += 1;
        }
    }
    Ok(placed)
}

/// Расстановка для несущей машины в своей транзакции: триггеры вне транзакции задач.
pub async fn place_carrier(
    pool: &sqlx::PgPool,
    registry: &Registry,
    carrier: Uuid,
) -> Result<usize, sqlx::Error> {
    let registry = registry.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move { place_waiting_in_tx(conn, &registry, carrier, Uuid::now_v7()).await })
    })
    .await
}

/// Несущие, у которых есть ожидающие раннер задачи.
pub async fn carriers_with_waiting(pool: &sqlx::PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select distinct carrier_id from jobs where state='waiting_runner' and carrier_id is not null",
    )
    .fetch_all(pool)
    .await
}

/// Несущая машины: она сама, если у неё есть ожидающие задачи, или родитель раннера. Нужна
/// триггерам по машине (подключение, выключатель).
pub async fn carrier_of(pool: &sqlx::PgPool, machine: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select case when role='runner' then parent_machine_id else id end
         from machines where id=$1",
    )
    .bind(machine)
    .fetch_optional(pool)
    .await
    .map(Option::flatten)
}
