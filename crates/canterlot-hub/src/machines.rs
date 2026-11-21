use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub struct NewMachine {
    pub machine_id: Uuid,
    pub token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RotateTokenError {
    #[error("machine_revoked")]
    MachineRevoked,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

pub async fn add(pool: &PgPool, owner_id: Uuid, name: &str) -> Result<NewMachine, sqlx::Error> {
    let token = new_token()?;
    let digest = Sha256::digest(token.as_bytes());
    let machine_id = Uuid::now_v7();
    let name = name.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("insert into machines (id, owner_id, name) values ($1, $2, $3)")
                .bind(machine_id)
                .bind(owner_id)
                .bind(name)
                .execute(&mut *conn)
                .await?;
            sqlx::query(
                "insert into machine_credentials (machine_id, token_sha256) values ($1, $2)",
            )
            .bind(machine_id)
            .bind(digest.as_slice())
            .execute(&mut *conn)
            .await?;
            Ok(NewMachine { machine_id, token })
        })
    })
    .await
}

pub async fn verify(pool: &PgPool, token: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let digest = Sha256::digest(token.as_bytes());
    let credentials: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
        "select machine_id, token_sha256 from machine_credentials where revoked_at is null",
    )
    .fetch_all(pool)
    .await?;
    let mut found = None;
    for (machine_id, stored) in credentials {
        if bool::from(stored.as_slice().ct_eq(digest.as_slice())) {
            found = Some(machine_id);
        }
    }
    Ok(found)
}

fn new_token() -> Result<String, sqlx::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| sqlx::Error::Io(std::io::Error::other("random_source_failed")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Отзыв токена машины. Задачи на месте этой машины, ещё не законченные (и стоящие в очереди),
/// заканчиваются `lost` в той же транзакции, каждая с одним сообщением об итоге: машины без
/// токена больше нет, и ждать её фактов нечего.
pub async fn revoke(pool: &PgPool, machine_id: Uuid) -> Result<bool, sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Замок машины задач раньше строки машины: тот же порядок, что у службы задач.
            crate::inplace_jobs::lock_machine(&mut *conn, machine_id).await?;
            // NO KEY UPDATE сериализует отзыв и ротацию, не блокируя FK нового курсора журнала.
            sqlx::query("select id from machines where id = $1 for no key update")
                .bind(machine_id)
                .fetch_optional(&mut *conn)
                .await?;
            let revoked = sqlx::query(
                "update machine_credentials set revoked_at = clock_timestamp()
                 where machine_id = $1 and revoked_at is null",
            )
            .bind(machine_id)
            .execute(&mut *conn)
            .await?
            .rows_affected()
                != 0;
            if revoked {
                notify_revoke(&mut *conn, machine_id).await?;
            }
            lose_jobs_in_tx(&mut *conn, machine_id).await?;
            Ok(revoked)
        })
    })
    .await
}

/// Незаконченные задачи на месте машины становятся `lost`; законченные не трогаются. Так же
/// заканчиваются ждущие раннер задачи этой машины как несущей (`carrier_id`) и задачи,
/// названные на неё и ждущие места: машины больше нет, ждать некого.
async fn lose_jobs_in_tx(
    conn: &mut sqlx::PgConnection,
    machine_id: Uuid,
) -> Result<(), sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from jobs where
           (machine_id = $1
              and state in ('staging','queued','dispatched','waiting_lease','running'))
           or (state = 'waiting_runner' and (carrier_id = $1 or requested_machine_id = $1))
         order by created_at, id",
    )
    .bind(machine_id)
    .fetch_all(&mut *conn)
    .await?;
    finish_lost_in_tx(conn, ids).await
}

/// Снятие пометки несущей: её ждущие раннер задачи (`carrier_id`) кончаются `lost`. Задачи,
/// уже идущие на её раннерах или на ней самой, остаются: им нужна только живая связь.
async fn lose_waiting_of_carrier_in_tx(
    conn: &mut sqlx::PgConnection,
    carrier: Uuid,
) -> Result<(), sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from jobs where state = 'waiting_runner' and carrier_id = $1
         order by created_at, id",
    )
    .bind(carrier)
    .fetch_all(&mut *conn)
    .await?;
    finish_lost_in_tx(conn, ids).await
}

/// Каждая задача кончается `lost` ровно один раз и с одним сообщением: законченная пропускается.
async fn finish_lost_in_tx(
    conn: &mut sqlx::PgConnection,
    ids: Vec<Uuid>,
) -> Result<(), sqlx::Error> {
    use crate::inplace_jobs::{Outcome, State, finish_in_tx, lock_row};
    for id in ids {
        let Some(job) = lock_row(&mut *conn, id).await? else {
            continue;
        };
        if job.is_terminal() {
            continue;
        }
        finish_in_tx(
            &mut *conn,
            &job,
            Outcome::bare(State::Lost, Some("lost")),
            Uuid::now_v7(),
        )
        .await?;
    }
    Ok(())
}

pub async fn rotate_token(
    pool: &PgPool,
    machine_id: Uuid,
    reset_local_state: bool,
    reactivate: bool,
) -> Result<Option<String>, RotateTokenError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            rotate_token_in_tx(conn, machine_id, reset_local_state, reactivate).await
        })
    })
    .await
}

async fn rotate_token_in_tx(
    conn: &mut sqlx::PgConnection,
    machine_id: Uuid,
    reset_local_state: bool,
    reactivate: bool,
) -> Result<Option<String>, RotateTokenError> {
    let token = new_token()?;
    let digest = Sha256::digest(token.as_bytes());
    if sqlx::query("select id from machines where id = $1 for no key update")
        .bind(machine_id)
        .fetch_optional(&mut *conn)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let revoked = sqlx::query("update machine_credentials set revoked_at = clock_timestamp() where machine_id = $1 and revoked_at is null")
        .bind(machine_id).execute(&mut *conn).await?.rows_affected();
    if revoked == 0 && !reactivate {
        return Err(RotateTokenError::MachineRevoked);
    }
    sqlx::query("insert into machine_credentials (machine_id, token_sha256) values ($1, $2)")
        .bind(machine_id)
        .bind(digest.as_slice())
        .execute(&mut *conn)
        .await?;
    if reset_local_state {
        sqlx::query("update machines set cj_journal_id = null where id = $1")
            .bind(machine_id)
            .execute(&mut *conn)
            .await?;
    }
    notify_revoke(&mut *conn, machine_id).await?;
    Ok(Some(token))
}

async fn notify_revoke(conn: &mut sqlx::PgConnection, machine_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_notify('canterlot_revoke', $1)")
        .bind(machine_id.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

/// Единственная точка допуска удалённой работы: принимает ли машина делегатов и команды
/// с других машин. Читает строку `for share` в транзакции запуска или диспетчеризации, а
/// смена выключателя берёт её `for update` (`set_accepts_remote_work`): после выключения
/// новый удалённый запуск не пройдёт, а уже закоммиченный проходит. Нет строки — `false`.
pub async fn accepts_remote_work_for_share(
    conn: &mut sqlx::PgConnection,
    machine_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let accepts: Option<bool> =
        sqlx::query_scalar("select accepts_remote_work from machines where id = $1 for share")
            .bind(machine_id)
            .fetch_optional(conn)
            .await?;
    Ok(accepts.unwrap_or(false))
}

/// Допуск удалённой работы вместе с поколением: тем же чтением `for share`, что и
/// `accepts_remote_work_for_share`. Поколение растёт при каждом выключении (триггер миграции
/// 0126), так что работа, запомнившая прежнее поколение, после повторного включения не
/// проходит. Нет строки — выключено, поколение 0.
pub async fn remote_admission_for_share(
    conn: &mut sqlx::PgConnection,
    machine_id: Uuid,
) -> Result<(bool, i64), sqlx::Error> {
    let row: Option<(bool, i64)> = sqlx::query_as(
        "select accepts_remote_work, remote_admission_generation from machines
         where id = $1 for share",
    )
    .bind(machine_id)
    .fetch_optional(conn)
    .await?;
    Ok(row.unwrap_or((false, 0)))
}

/// Меняет выключатель удалённой работы; только HTTP владельца. `None` — машины нет у владельца.
pub async fn set_accepts_remote_work(
    conn: &mut sqlx::PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    accepts: bool,
) -> Result<Option<bool>, sqlx::Error> {
    // Владелец раньше строки машины: тот же порядок, что у запуска делегата.
    crate::conversations::lock_owner(conn, owner_id).await?;
    let current: Option<bool> = sqlx::query_scalar(
        "select accepts_remote_work from machines where id = $1 and owner_id = $2 for update",
    )
    .bind(machine_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    if current != accepts {
        sqlx::query("update machines set accepts_remote_work = $2 where id = $1")
            .bind(machine_id)
            .bind(accepts)
            .execute(&mut *conn)
            .await?;
    }
    Ok(Some(accepts))
}

// ───────── Раннеры как машины ─────────

/// Роль строки `machines`: обычная машина с воркером или раннер, которого ведёт несущая.
pub const ROLE_WORKER: &str = "worker";
pub const ROLE_RUNNER: &str = "runner";

/// Предел корня раннера, как у несущей (`script::valid_root`): путь сокета `<root>/hub.sock`
/// обязан влезть в `sun_path`.
const MAX_ROOT_DIR_BYTES: usize = 90;
const MAX_LABELS: usize = 32;
const MAX_LABEL_BYTES: usize = 64;

/// Что владелец задаёт при создании раннера (`machine add --role runner`).
#[derive(Clone, Debug)]
pub struct RunnerSpec {
    pub parent: Uuid,
    pub root_dir: String,
    pub rank: i32,
    pub labels: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AddRunnerError {
    #[error("parent_not_found")]
    ParentNotFound,
    /// Родитель сам раннер: несущей может быть только машина с ролью `worker`.
    #[error("parent_not_worker")]
    ParentNotWorker,
    #[error("parent_revoked")]
    ParentRevoked,
    #[error("root_dir_invalid")]
    InvalidRootDir,
    #[error("labels_invalid")]
    InvalidLabels,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

/// Корень раннера попадает в командные строки несущей и в сгенерированный `worker.toml`,
/// поэтому принимается только абсолютный нормализованный путь из безопасных знаков.
pub fn valid_root_dir(path: &str) -> bool {
    path.len() <= MAX_ROOT_DIR_BYTES
        && path.starts_with('/')
        && path != "/"
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b'@')
        })
        && path[1..]
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn valid_labels(labels: &[String]) -> bool {
    labels.len() <= MAX_LABELS
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= MAX_LABEL_BYTES
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// Создаёт раннера: строка `machines` с ролью `runner`, родителем-несущей, корнем, рангом и
/// метками, и свой токен. Раннер принимает удалённую работу сразу: выключатель владельца у него
/// нужен как «пауза», а не как допуск. Родитель — существующая неотозванная машина `worker`.
pub async fn add_runner(
    pool: &PgPool,
    owner_id: Uuid,
    name: &str,
    spec: &RunnerSpec,
) -> Result<NewMachine, AddRunnerError> {
    if !valid_root_dir(&spec.root_dir) {
        return Err(AddRunnerError::InvalidRootDir);
    }
    if !valid_labels(&spec.labels) {
        return Err(AddRunnerError::InvalidLabels);
    }
    let token = new_token()?;
    let digest = Sha256::digest(token.as_bytes());
    let machine_id = Uuid::now_v7();
    let name = name.to_owned();
    let spec = spec.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // FOR SHARE: отзыв родителя берёт NO KEY UPDATE и дождётся этой транзакции.
            let parent: Option<(String, bool)> = sqlx::query_as(
                "select role, exists(select 1 from machine_credentials c
                                      where c.machine_id = m.id and c.revoked_at is null)
                 from machines m where id = $1 and owner_id = $2 for share",
            )
            .bind(spec.parent)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            match parent {
                None => return Err(AddRunnerError::ParentNotFound),
                Some((role, _)) if role != ROLE_WORKER => {
                    return Err(AddRunnerError::ParentNotWorker);
                }
                Some((_, false)) => return Err(AddRunnerError::ParentRevoked),
                Some(_) => {}
            }
            sqlx::query(
                "insert into machines (id, owner_id, name, role, parent_machine_id, root_dir,
                                       rank, labels, accepts_remote_work)
                 values ($1, $2, $3, 'runner', $4, $5, $6, $7, true)",
            )
            .bind(machine_id)
            .bind(owner_id)
            .bind(name)
            .bind(spec.parent)
            .bind(&spec.root_dir)
            .bind(spec.rank)
            .bind(&spec.labels)
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "insert into machine_credentials (machine_id, token_sha256) values ($1, $2)",
            )
            .bind(machine_id)
            .bind(digest.as_slice())
            .execute(&mut *conn)
            .await?;
            Ok(NewMachine { machine_id, token })
        })
    })
    .await
}

/// Что `machine set` меняет у машины; пустое поле остаётся как есть.
#[derive(Clone, Debug, Default)]
pub struct MachineAttrs {
    /// Ранг размещения раннера: больше — предпочтительнее. У несущей учитывается как 0.
    pub rank: Option<i32>,
    /// Метки целиком (пустой список снимает все).
    pub labels: Option<Vec<String>>,
    /// Корень раннера: только у раннера и только пока на нём нет незаконченных задач.
    pub root_dir: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SetMachineError {
    #[error("machine_not_found")]
    NotFound,
    #[error("nothing_to_set")]
    NothingToSet,
    #[error("labels_invalid")]
    InvalidLabels,
    #[error("root_dir_invalid")]
    InvalidRootDir,
    /// Корень есть только у раннера.
    #[error("root_dir_needs_runner")]
    RootDirNeedsRunner,
    /// Смена корня под идущими задачами оставила бы на старом месте их каталоги.
    #[error("runner_has_unfinished_jobs")]
    RunnerHasJobs,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

/// Меняет ранг, метки и корень машины (`machine set`): владелец метит и саму VM (например
/// `kernel-6.12`), и раннеры. Только CLI владельца.
pub async fn set_attrs(
    pool: &PgPool,
    machine_id: Uuid,
    attrs: &MachineAttrs,
) -> Result<(), SetMachineError> {
    if attrs.rank.is_none() && attrs.labels.is_none() && attrs.root_dir.is_none() {
        return Err(SetMachineError::NothingToSet);
    }
    if attrs
        .labels
        .as_ref()
        .is_some_and(|labels| !valid_labels(labels))
    {
        return Err(SetMachineError::InvalidLabels);
    }
    if attrs
        .root_dir
        .as_deref()
        .is_some_and(|root| !valid_root_dir(root))
    {
        return Err(SetMachineError::InvalidRootDir);
    }
    let attrs = attrs.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Замок машины задач раньше строки машины, как у отзыва.
            crate::inplace_jobs::lock_machine(&mut *conn, machine_id).await?;
            let role: Option<String> =
                sqlx::query_scalar("select role from machines where id = $1 for update")
                    .bind(machine_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            let Some(role) = role else {
                return Err(SetMachineError::NotFound);
            };
            if let Some(root) = &attrs.root_dir {
                if role != ROLE_RUNNER {
                    return Err(SetMachineError::RootDirNeedsRunner);
                }
                let busy: bool = sqlx::query_scalar(
                    "select exists(select 1 from jobs where machine_id = $1
                       and state not in ('succeeded','failed','timed_out','canceled','lost'))",
                )
                .bind(machine_id)
                .fetch_one(&mut *conn)
                .await?;
                let current: Option<String> =
                    sqlx::query_scalar("select root_dir from machines where id = $1")
                        .bind(machine_id)
                        .fetch_one(&mut *conn)
                        .await?;
                if busy && current.as_deref() != Some(root.as_str()) {
                    return Err(SetMachineError::RunnerHasJobs);
                }
                sqlx::query("update machines set root_dir = $2 where id = $1")
                    .bind(machine_id)
                    .bind(root)
                    .execute(&mut *conn)
                    .await?;
            }
            if let Some(rank) = attrs.rank {
                sqlx::query("update machines set rank = $2 where id = $1")
                    .bind(machine_id)
                    .bind(rank)
                    .execute(&mut *conn)
                    .await?;
            }
            if let Some(labels) = &attrs.labels {
                sqlx::query("update machines set labels = $2 where id = $1")
                    .bind(machine_id)
                    .bind(labels)
                    .execute(&mut *conn)
                    .await?;
            }
            // Метки и ранг меняют выбор места: ждущим задачам пора на расстановку.
            crate::inplace_jobs::carrier::notify_place_for_machine(&mut *conn, Some(machine_id))
                .await?;
            Ok(())
        })
    })
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum SetCarrierError {
    #[error("machine_not_found")]
    NotFound,
    /// Несущей может быть только машина с ролью `worker`.
    #[error("machine_is_runner")]
    IsRunner,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

/// Помечает машину несущей (или снимает пометку): только она может сообщать о раннерах,
/// просить их токены и принимать команды стейджинга. Только CLI владельца.
pub async fn set_carrier(pool: &PgPool, machine_id: Uuid, on: bool) -> Result<(), SetCarrierError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Замок машины задач раньше строки машины, как у отзыва.
            crate::inplace_jobs::lock_machine(&mut *conn, machine_id).await?;
            let role: Option<String> =
                sqlx::query_scalar("select role from machines where id = $1 for update")
                    .bind(machine_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            match role.as_deref() {
                None => return Err(SetCarrierError::NotFound),
                Some(ROLE_RUNNER) if on => return Err(SetCarrierError::IsRunner),
                Some(_) => {}
            }
            sqlx::query("update machines set is_carrier = $2 where id = $1")
                .bind(machine_id)
                .bind(on)
                .execute(&mut *conn)
                .await?;
            if !on {
                // Без несущей ждущие её раннер задачи никто не расставит: они кончаются `lost`.
                lose_waiting_of_carrier_in_tx(&mut *conn, machine_id).await?;
            }
            Ok(())
        })
    })
    .await
}

/// Отказ несущей: код уходит воркеру в `RunnerTokenRefused`. `Internal` — сбой базы, наружу
/// кодом не отдаётся.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RunnerRefusal {
    #[error("not_carrier")]
    NotCarrier,
    #[error("not_parent")]
    NotParent,
    #[error("revoked")]
    Revoked,
    #[error("unknown_machine")]
    UnknownMachine,
    #[error("internal")]
    Internal,
}

impl RunnerRefusal {
    /// Код для `RunnerTokenRefused.code`; `None` у сбоя базы.
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::NotCarrier => Some("not_carrier"),
            Self::NotParent => Some("not_parent"),
            Self::Revoked => Some("revoked"),
            Self::UnknownMachine => Some("unknown_machine"),
            Self::Internal => None,
        }
    }
}

impl From<sqlx::Error> for RunnerRefusal {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(
            kind = "runner_gate_database",
            error = %crate::dberr::describe(&error),
            "runner gate query failed"
        );
        Self::Internal
    }
}

impl From<RotateTokenError> for RunnerRefusal {
    fn from(error: RotateTokenError) -> Self {
        match error {
            RotateTokenError::MachineRevoked => Self::Revoked,
            RotateTokenError::Database(error) => error.into(),
        }
    }
}

/// Допуск несущей: единственная точка, общая для запроса токена и наблюдений. Вызывающая —
/// машина с ролью `worker` и флагом `is_carrier`; раннер — строка с ролью `runner`, родитель
/// которой вызывающая. Строка вызывающей берётся `for share` (снятие флага берёт её `for
/// update`), строка раннера — `for no key update`, как у ротации и отзыва.
async fn carrier_gate(
    conn: &mut sqlx::PgConnection,
    caller: Uuid,
    runner: Uuid,
) -> Result<(), RunnerRefusal> {
    let carrier: Option<bool> = sqlx::query_scalar(
        "select role = 'worker' and is_carrier from machines where id = $1 for share",
    )
    .bind(caller)
    .fetch_optional(&mut *conn)
    .await?;
    if carrier != Some(true) {
        return Err(RunnerRefusal::NotCarrier);
    }
    let parent: Option<Option<Uuid>> = sqlx::query_scalar(
        "select parent_machine_id from machines where id = $1 and role = 'runner'
         for no key update",
    )
    .bind(runner)
    .fetch_optional(&mut *conn)
    .await?;
    match parent {
        None => Err(RunnerRefusal::UnknownMachine),
        Some(parent) if parent != Some(caller) => Err(RunnerRefusal::NotParent),
        Some(_) => Ok(()),
    }
}

/// Токен раннера по запросу несущей: допуск, затем `rotate_token(.., reactivate = false)`.
/// Отозванный раннер не оживает (`Revoked`). Значение вернётся только вызывающему и нигде не
/// сохраняется.
pub async fn request_runner_token(
    pool: &PgPool,
    caller: Uuid,
    runner: Uuid,
    reset_local_state: bool,
) -> Result<String, RunnerRefusal> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            carrier_gate(&mut *conn, caller, runner).await?;
            rotate_token_in_tx(&mut *conn, runner, reset_local_state, false)
                .await?
                .ok_or(RunnerRefusal::UnknownMachine)
        })
    })
    .await
}

/// Наблюдение несущей за раннером: после того же допуска последнее значение заменяет прежнее.
/// Время не позже «сейчас»: часы несущей не должны делать наблюдение вечно свежим.
pub async fn record_runner_observation(
    pool: &PgPool,
    caller: Uuid,
    runner: Uuid,
    reachable: bool,
    consecutive_failures: u32,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), RunnerRefusal> {
    let observed_at = observed_at.min(chrono::Utc::now());
    let failures = i32::try_from(consecutive_failures).unwrap_or(i32::MAX);
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            carrier_gate(&mut *conn, caller, runner).await?;
            sqlx::query(
                "insert into runner_observations
                   (machine_id, reachable, consecutive_failures, observed_at, carrier_id)
                 values ($1, $2, $3, $4, $5)
                 on conflict (machine_id) do update set
                   reachable = excluded.reachable,
                   consecutive_failures = excluded.consecutive_failures,
                   observed_at = excluded.observed_at,
                   carrier_id = excluded.carrier_id",
            )
            .bind(runner)
            .bind(reachable)
            .bind(failures)
            .bind(observed_at)
            .bind(caller)
            .execute(&mut *conn)
            .await?;
            Ok(())
        })
    })
    .await
}

/// Раннер в списке несущей.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AssignedRunner {
    pub machine_id: Uuid,
    pub name: String,
    pub root_dir: String,
}

/// Неотозванные раннеры-потомки машины, по имени. Отозванный раннер из списка уходит, и
/// несущая останавливает его воркер.
pub async fn runners_of(pool: &PgPool, carrier: Uuid) -> Result<Vec<AssignedRunner>, sqlx::Error> {
    sqlx::query_as(
        "select m.id machine_id, m.name, m.root_dir from machines m
         where m.parent_machine_id = $1 and m.role = 'runner'
           and exists(select 1 from machine_credentials c
                       where c.machine_id = m.id and c.revoked_at is null)
         order by m.name, m.id",
    )
    .bind(carrier)
    .fetch_all(pool)
    .await
}

/// Доступность раннера для клиентов и для размещения (этап C берёт то же правило).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerAvailability {
    Online,
    Offline,
    Unknown,
}

impl RunnerAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Unknown => "unknown",
        }
    }
}

/// Наблюдение считается свежим, если ему меньше трёх минут.
pub const OBSERVATION_FRESH: chrono::TimeDelta = chrono::TimeDelta::minutes(3);
/// Столько неудач подряд нужно, чтобы наблюдение говорило «недоступен».
pub const OFFLINE_FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy)]
pub struct RunnerObservationFact {
    pub reachable: bool,
    pub consecutive_failures: u32,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy)]
pub struct RunnerFacts {
    /// Воркер самого раннера подключён.
    pub runner_connected: bool,
    /// Воркер его несущей (родителя) подключён.
    pub carrier_connected: bool,
    /// Последнее наблюдение именно родителя.
    pub observation: Option<RunnerObservationFact>,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// Правило доступности раннера, одно на весь хаб.
/// - `online`: воркер раннера подключён;
/// - `offline`: только когда он отключён И несущая подключена И наблюдение свежее (моложе
///   трёх минут) И в нём не меньше трёх неудач подряд;
/// - иначе `unknown`.
pub fn runner_availability(facts: &RunnerFacts) -> RunnerAvailability {
    if facts.runner_connected {
        return RunnerAvailability::Online;
    }
    let confirmed_down = facts.carrier_connected
        && facts.observation.is_some_and(|observation| {
            !observation.reachable
                && observation.consecutive_failures >= OFFLINE_FAILURES
                && facts.now - observation.observed_at < OBSERVATION_FRESH
        });
    if confirmed_down {
        RunnerAvailability::Offline
    } else {
        RunnerAvailability::Unknown
    }
}

/// Доступность раннера по реестру сеансов и сохранённому наблюдению; второе значение — когда
/// оно сделано (для клиентов: возраст наблюдения). Наблюдение чужой несущей (раннер сменил
/// родителя) не считается.
pub async fn runner_availability_of(
    conn: &mut sqlx::PgConnection,
    registry: &crate::session::Registry,
    runner: Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(RunnerAvailability, Option<chrono::DateTime<chrono::Utc>>), sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        parent_machine_id: Option<Uuid>,
        reachable: Option<bool>,
        consecutive_failures: Option<i32>,
        observed_at: Option<chrono::DateTime<chrono::Utc>>,
    }
    let row: Option<Row> = sqlx::query_as(
        "select m.parent_machine_id, o.reachable, o.consecutive_failures, o.observed_at
             from machines m
             left join runner_observations o
               on o.machine_id = m.id and o.carrier_id = m.parent_machine_id
             where m.id = $1 and m.role = 'runner'",
    )
    .bind(runner)
    .fetch_optional(conn)
    .await?;
    let Some(Row {
        parent_machine_id: parent,
        reachable,
        consecutive_failures: failures,
        observed_at,
    }) = row
    else {
        return Ok((RunnerAvailability::Unknown, None));
    };
    let connected =
        |id: Uuid| registry.availability(id) != crate::session::Availability::Unavailable;
    let observation = match (reachable, failures, observed_at) {
        (Some(reachable), Some(failures), Some(observed_at)) => Some(RunnerObservationFact {
            reachable,
            consecutive_failures: u32::try_from(failures).unwrap_or(0),
            observed_at,
        }),
        _ => None,
    };
    let facts = RunnerFacts {
        runner_connected: connected(runner),
        carrier_connected: parent.is_some_and(connected),
        observation,
        now,
    };
    Ok((runner_availability(&facts), observed_at))
}

pub(crate) async fn credential_active(
    conn: &mut sqlx::PgConnection,
    machine_id: Uuid,
    digest: &[u8],
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from machine_credentials where machine_id = $1 and token_sha256 = $2 and revoked_at is null)")
        .bind(machine_id).bind(digest).fetch_one(conn).await
}
