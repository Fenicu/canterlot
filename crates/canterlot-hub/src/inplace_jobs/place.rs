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
pub(crate) struct Candidate {
    id: Uuid,
    rank: i32,
    labels: Vec<String>,
    root_dir: Option<String>,
    accepts_remote_work: bool,
    capabilities: Vec<String>,
    /// Строка облачного раннера: единственное место с видом `cloud`.
    is_cloud: bool,
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
        let kind = if self.is_cloud {
            PLACEMENT_CLOUD
        } else if self.is_carrier(carrier) {
            PLACEMENT_LOCAL
        } else {
            PLACEMENT_RUNNER
        };
        placement.iter().any(|allowed| allowed == kind)
    }
}

async fn candidates(conn: &mut PgConnection, carrier: Uuid) -> Result<Vec<Candidate>, sqlx::Error> {
    sqlx::query_as(
        "select m.id, m.rank, m.labels, m.root_dir, m.accepts_remote_work, m.capabilities,
                exists(select 1 from cloud_budget b where b.cloud_machine_id = m.id) as is_cloud
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

/// Замки машин несущей и её раннеров: все сразу и по возрастанию идентификатора. Заявка `auto`
/// берёт их до допуска заказчика (`lock_admission`), как диспетчер машины
/// (`dispatch_machine_in_tx`: замок машины, потом допуск), поэтому пути не замыкают цикл.
/// Замок транзакционный и повторяемый: расстановка после этого берёт те же замки снова.
pub(crate) async fn lock_candidates(
    conn: &mut PgConnection,
    carrier: Uuid,
) -> Result<Vec<Candidate>, sqlx::Error> {
    let all = candidates(conn, carrier).await?;
    for candidate in &all {
        lock_machine(conn, candidate.id).await?;
    }
    Ok(all)
}

/// Задача, которую можно когда-нибудь разместить: есть настроенное место с её метками среди
/// разрешённых видов. Включённость, связь и свободные места не считаются: это отказ при
/// подаче, а не ожидание. Настроенный облачный профиль — тоже место (`cloud`): его строка
/// появляется только с первым окном, поэтому ожидание допустимо и без неё.
pub async fn satisfiable(
    conn: &mut PgConnection,
    carrier: Uuid,
    labels: &[String],
    placement: &[String],
    repo: &str,
    domain: &str,
    cloud_labels: Option<&[String]>,
) -> Result<bool, sqlx::Error> {
    // Несущая подходит, только если её рабочий каталог остаётся в домене исходников.
    let local_ok = carrier_workdir_in_domain(conn, carrier, repo, domain).await?;
    let by_machine = candidates(conn, carrier).await?.iter().any(|candidate| {
        candidate.capable()
            && candidate.allowed_by(carrier, placement)
            && candidate.has_labels(labels)
            && (local_ok || !candidate.is_carrier(carrier))
    });
    if by_machine {
        return Ok(true);
    }
    Ok(cloud_labels.is_some_and(|cloud| {
        placement.iter().any(|kind| kind == PLACEMENT_CLOUD)
            && labels.iter().all(|label| cloud.contains(label))
    }))
}

/// Ключ голого репозитория: имя каталога и 12 первых шестнадцатеричных знаков sha256 от
/// абсолютного пути. Одинаковые имена в разных местах не делят хранилище объектов.
///
/// Имя в ключе сводится к `[A-Za-z0-9._-]`: другие знаки (пробел, `+`, юникод) заменяются на
/// `-`, а точки и пустое имя (`.`, `..`, ``) дают `repo`. Хеш считается от настоящего пути, так
/// что разные пути с одинаково сведённым именем ключей не делят. Несущая принимает ровно этот
/// алфавит (`valid_key`), поэтому задача не доходит до подготовки и не падает на имени.
pub fn repo_key(repo: &str) -> String {
    let digest = Sha256::digest(repo.as_bytes());
    let hash: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let name: String = repo_dir_name(repo)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = if name.chars().all(|c| c == '.') {
        "repo".to_owned()
    } else {
        name
    };
    format!("{name}-{hash}")
}

/// Рабочий каталог задачи на самой несущей:
/// `<каталог над репозиторием>/.canterlot-jobs/<имя репозитория>/<id>`.
pub fn carrier_workdir(repo: &str, job: Uuid) -> String {
    let trimmed = repo.trim_end_matches('/');
    let parent = trimmed.rsplit_once('/').map_or("", |(parent, _)| parent);
    format!("{parent}/.canterlot-jobs/{}/{job}", repo_dir_name(repo))
}

/// Рабочий каталог на несущей остаётся в домене исходников? Он лежит над репозиторием, и если
/// сам репозиторий — рабочий корень, то каталог над ним уже по пути «личный». Такой задаче на
/// несущей места нет: она отказывается (`domain_mismatch`) или идёт только на раннер.
/// Проще, чем класть каталог внутрь репозитория и прятать его в `.git/info/exclude`.
pub async fn carrier_workdir_in_domain(
    conn: &mut PgConnection,
    carrier: Uuid,
    repo: &str,
    domain: &str,
) -> Result<bool, sqlx::Error> {
    let workdir = carrier_workdir(repo, Uuid::nil());
    let found = crate::memory::path_domain(conn, carrier, &workdir).await?;
    Ok(found.map(crate::memory::MemoryDomain::as_str) == Some(domain))
}

/// Рабочий каталог задачи: на раннере `<root>/jobs/<домен>/<id>`, на самой несущей
/// `carrier_workdir`.
pub fn workdir_for(job: &Job, root_dir: Option<&str>, on_carrier: bool) -> Option<String> {
    let repo = job.source_repo.as_deref()?;
    let domain = job.source_domain.as_deref()?;
    if on_carrier {
        Some(carrier_workdir(repo, job.id))
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

/// Метки задачи — подмножество меток машины. Для названной машины это условие допуска: имя
/// не отменяет требования к раннеру (например, ядро для проверки).
pub async fn machine_has_labels(
    conn: &mut PgConnection,
    machine: Uuid,
    labels: &[String],
) -> Result<bool, sqlx::Error> {
    let have: Option<Vec<String>> = sqlx::query_scalar("select labels from machines where id=$1")
        .bind(machine)
        .fetch_optional(conn)
        .await?;
    Ok(have.is_some_and(|have| labels.iter().all(|label| have.contains(label))))
}

/// Привязка к названной машине без выбора (`machine` назван в заявке). Есть свободное место —
/// задача идёт на подготовку; места нет — она остаётся в `waiting_runner` с названной машиной
/// и ждёт её, а не встаёт в очередь машины мимо проверки места. Возвращает, привязана ли.
/// Под замком машины.
pub async fn bind_explicit_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    carrier: Uuid,
    target: Uuid,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if occupied_in_tx(conn, target).await? >= max_running_in_tx(conn, target).await? {
        sqlx::query(
            "update jobs set requested_machine_id=$2, updated_at=clock_timestamp() where id=$1",
        )
        .bind(job.id)
        .bind(target)
        .execute(&mut *conn)
        .await?;
        return Ok(false);
    }
    let root: Option<String> = sqlx::query_scalar("select root_dir from machines where id=$1")
        .bind(target)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    bind_target_in_tx(conn, job, carrier, target, root.as_deref(), correlation_id).await?;
    Ok(true)
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
    let mut all = lock_candidates(conn, carrier).await?;
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
        let mut local_ok = None;
        for candidate in &all {
            // Рабочий каталог на несущей обязан остаться в домене исходников.
            if candidate.is_carrier(carrier) {
                if local_ok.is_none() {
                    local_ok = Some(
                        carrier_workdir_in_domain(
                            conn,
                            carrier,
                            job.source_repo.as_deref().unwrap_or_default(),
                            job.source_domain.as_deref().unwrap_or_default(),
                        )
                        .await?,
                    );
                }
                if local_ok == Some(false) {
                    continue;
                }
            }
            // Названная машина берёт только свою задачу, виды места на неё не действуют.
            let allowed = match job.requested_machine_id {
                Some(requested) => candidate.id == requested,
                None => candidate.allowed_by(carrier, &job.placement),
            };
            if !candidate.capable()
                || !allowed
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
