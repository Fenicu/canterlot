//! Сборщик вложений: просроченные загрузки, неиспользованные готовые вложения и orphan-файлы.
//! Каждое решение берёт строку `for update skip locked` и перепроверяет условия после блокировки.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use super::{BlobStore, FileKind, linked, state_event};
use crate::config::AttachmentsConfig;

const BATCH: i64 = 1000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GcReport {
    pub expired_uploads: usize,
    pub released: usize,
    pub orphan_blobs: usize,
    pub orphan_staging: usize,
    /// Опубликованные картинки агента, на которые так и не сослался блок.
    pub unreferenced_media: usize,
    /// Lock-файлы, за которыми не осталось ни строки в базе, ни файлов.
    pub stale_locks: usize,
}

/// Условие «журнал исполнения ещё может прикрепить строку `alias`». Воркер сдаёт файл
/// (`PutMedia`) раньше, чем пишет о нём запись в журнал сопровождения, а разрыв между
/// ними бывает дольше отсрочки сборщика: журнал стоит на записи, которую хаб не смог
/// применить, или машина сутки без связи. Удалённую строку поздняя запись молча
/// пропускает, и файл в ленте не появляется. Поэтому неприкреплённое не трогаем, пока
/// исполнение живо или его журнал не закрыт: после закрытия записей в нём не будет.
fn journal_pending(alias: &str) -> String {
    format!(
        "(exists(select 1 from executions e where e.id={alias}.execution_id
                 and e.state not in ('stopped','failed'))
          or exists(select 1 from journal_cursors j where j.execution_id={alias}.execution_id
                    and j.kind='sj' and not j.closed and not j.discarded))"
    )
}

/// Файл агента (`block_files`) этого вложения ещё ждёт своей записи журнала.
async fn file_awaits_journal(conn: &mut sqlx::PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select exists(select 1 from block_files bf where bf.attachment_id=$1
                       and bf.attached_at is null and {})",
        journal_pending("bf")
    )))
    .bind(id)
    .fetch_one(conn)
    .await
}

/// ID, у которых в базе есть строка вложения, медиа или журнала задачи.
async fn cataloged(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select ids.id from unnest($1::uuid[]) ids(id)
         where exists(select 1 from attachments a where a.id=ids.id)
            or exists(select 1 from block_media m where m.id=ids.id)
            or exists(select 1 from jobs j where j.log_media_id=ids.id)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
}

/// Lock-файл создаётся на каждый принятый файл и сам не исчезает: после удаления разговора
/// от вложения остаётся пустой файл, и каталог рос бы без предела. Убираем те, что старше
/// отсрочки и за которыми нет ни строки, ни staging, ни blob. Удаление идёт под захватом
/// этого же файла и с повторной проверкой: приём с тем же ID либо держит захват, либо
/// придёт позже и создаст новый файл (`try_lock` не принимает захват на удалённом).
async fn sweep_locks(
    pool: &PgPool,
    store: &dyn BlobStore,
    cutoff: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let locks = match store.scan_locks().await {
        Ok(locks) => locks,
        Err(error) => {
            tracing::warn!(kind = "attachment_gc_scan_failed", error = %error.kind(), "lock scan failed");
            return Ok(0);
        }
    };
    let old: Vec<Uuid> = locks
        .into_iter()
        .filter(|(_, created)| *created < cutoff)
        .map(|(id, _)| id)
        .collect();
    let mut removed = 0;
    for batch in old.chunks(BATCH as usize) {
        let known: std::collections::HashSet<Uuid> =
            cataloged(pool, batch).await?.into_iter().collect();
        for id in batch.iter().filter(|id| !known.contains(id)) {
            let Some(locked) = store.try_lock(*id).map_err(sqlx::Error::Io)? else {
                continue;
            };
            // Под захватом: строка могла появиться, а файлы — лежать без строки (приём
            // оборвался между публикацией и commit; их уберёт обход orphan-файлов).
            let files = locked
                .staging_len(*id)
                .await
                .map_err(sqlx::Error::Io)?
                .is_some()
                || !matches!(locked.open(*id, 0).await, Err(super::BlobError::NotFound));
            if files || !cataloged(pool, &[*id]).await?.is_empty() {
                continue;
            }
            match locked.delete_lock().await {
                Ok(()) => removed += 1,
                Err(error) => log_file_error(*id, &error),
            }
        }
    }
    Ok(removed)
}

fn log_file_error(id: Uuid, error: &std::io::Error) {
    tracing::warn!(kind = "attachment_gc_delete_failed", attachment_id = %id, error = %error.kind(), "file delete failed; retried by the orphan sweep");
}

/// Загрузка без активности дольше отсрочки становится `failed(upload_expired)`.
async fn expire(
    pool: &PgPool,
    store: &dyn BlobStore,
    id: Uuid,
    cutoff: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    // Активное тело в любом процессе не трогаем.
    let Some(locked) = store.try_lock(id).map_err(sqlx::Error::Io)? else {
        return Ok::<bool, sqlx::Error>(false);
    };
    let store = &*locked;
    let expired = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let row: Option<(String, DateTime<Utc>, Uuid)> = sqlx::query_as(
                "select state, updated_at, conversation_id from attachments where id=$1 for update skip locked",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            let Some((state, updated_at, conversation_id)) = row else {
                return Ok::<bool, sqlx::Error>(false);
            };
            if !matches!(state.as_str(), "created" | "uploading") || updated_at >= cutoff {
                return Ok::<bool, sqlx::Error>(false);
            }
            sqlx::query(
                "update attachments set state='failed', fail_reason='upload_expired', updated_at=clock_timestamp() where id=$1",
            )
            .bind(id)
            .execute(&mut *conn)
            .await?;
            state_event(
                &mut *conn,
                conversation_id,
                id,
                "failed",
                Some("upload_expired"),
                Uuid::now_v7(),
            )
            .await?;
            Ok(true)
        })
    })
    .await?;
    if expired && let Err(error) = store.delete_staging(id).await {
        log_file_error(id, &error);
    }
    Ok(expired)
}

/// Готовое вложение без ссылок становится `released`; blob удаляется только после commit.
async fn release(
    pool: &PgPool,
    store: &dyn BlobStore,
    id: Uuid,
    cutoff: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Some(locked) = store.try_lock(id).map_err(sqlx::Error::Io)? else {
        return Ok::<bool, sqlx::Error>(false);
    };
    let store = &*locked;
    let released = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let row: Option<(String, DateTime<Utc>, Uuid)> = sqlx::query_as(
                "select state, updated_at, conversation_id from attachments where id=$1 for update skip locked",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            let Some((state, updated_at, conversation_id)) = row else {
                return Ok::<bool, sqlx::Error>(false);
            };
            // Новый оператор после блокировки видит ссылку, зафиксированную конкурентной привязкой.
            if state != "ready"
                || updated_at >= cutoff
                || linked(&mut *conn, id).await?
                || file_awaits_journal(&mut *conn, id).await?
            {
                return Ok::<bool, sqlx::Error>(false);
            }
            sqlx::query(
                "update attachments set state='released', updated_at=clock_timestamp() where id=$1",
            )
            .bind(id)
            .execute(&mut *conn)
            .await?;
            // Файл агента без ссылки из блока: поздняя ссылка указывала бы на удалённый blob.
            sqlx::query("delete from block_files where attachment_id=$1 and attached_at is null")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            state_event(
                &mut *conn,
                conversation_id,
                id,
                "released",
                None,
                Uuid::now_v7(),
            )
            .await?;
            Ok(true)
        })
    })
    .await?;
    if released && let Err(error) = store.delete_blob(id).await {
        log_file_error(id, &error);
    }
    Ok(released)
}

/// Картинка без ссылки из блока дольше отсрочки удаляется: строка первой, blob после commit.
async fn drop_unreferenced_media(
    pool: &PgPool,
    store: &dyn BlobStore,
    id: Uuid,
    cutoff: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    // Приём того же ID в любом процессе не трогаем.
    let Some(locked) = store.try_lock(id).map_err(sqlx::Error::Io)? else {
        return Ok::<bool, sqlx::Error>(false);
    };
    let store = &*locked;
    let dropped = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let locked: Option<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "select m.id from block_media m
                 where m.id=$1 and m.attached_at is null and m.created_at < $2 and not {}
                 for update of m skip locked",
                journal_pending("m")
            )))
            .bind(id)
            .bind(cutoff)
            .fetch_optional(&mut *conn)
            .await?;
            if locked.is_none() {
                return Ok::<bool, sqlx::Error>(false);
            }
            sqlx::query("delete from block_media where id=$1")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            Ok(true)
        })
    })
    .await?;
    if dropped && let Err(error) = store.delete_blob(id).await {
        log_file_error(id, &error);
    }
    Ok(dropped)
}

pub async fn run_once(
    pool: &PgPool,
    store: &dyn BlobStore,
    now: DateTime<Utc>,
    grace: Duration,
) -> Result<GcReport, sqlx::Error> {
    let cutoff = now - TimeDelta::from_std(grace).unwrap_or(TimeDelta::MAX);
    let mut report = GcReport::default();
    let stale: Vec<Uuid> = sqlx::query_scalar(
        "select id from attachments where state in ('created','uploading') and updated_at < $1 order by id limit $2",
    )
    .bind(cutoff)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for id in stale {
        if expire(pool, store, id, cutoff).await? {
            report.expired_uploads += 1;
        }
    }
    // Ждущие журнала строки отсеиваются уже здесь: иначе тысяча таких вложений заняла бы
    // пачку целиком, и до остальных сборщик не дошёл бы.
    let unused: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select a.id from attachments a where a.state='ready' and a.updated_at < $1
           and not exists(select 1 from input_attachments ia where ia.attachment_id=a.id)
           and not exists(select 1 from scheduled_job_attachments sa where sa.attachment_id=a.id)
           and not exists(select 1 from block_files bf where bf.attachment_id=a.id
                          and (bf.attached_at is not null or {}))
           and not exists(select 1 from composer_drafts d where d.attachment_ids ? a.id::text)
         order by a.id limit $2",
        journal_pending("bf")
    )))
    .bind(cutoff)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for id in unused {
        if release(pool, store, id, cutoff).await? {
            report.released += 1;
        }
    }
    let unreferenced: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select m.id from block_media m
         where m.attached_at is null and m.created_at < $1 and not {}
         order by m.id limit $2",
        journal_pending("m")
    )))
    .bind(cutoff)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for id in unreferenced {
        if drop_unreferenced_media(pool, store, id, cutoff).await? {
            report.unreferenced_media += 1;
        }
    }
    let files = match store.scan().await {
        Ok(files) => files,
        Err(error) => {
            tracing::warn!(kind = "attachment_gc_scan_failed", error = %error.kind(), "blob scan failed");
            return Ok(report);
        }
    };
    let files: Vec<_> = files.into_iter().filter(|f| f.mtime < cutoff).collect();
    for batch in files.chunks(128) {
        let mut locked = std::collections::HashMap::new();
        for file in batch {
            if let std::collections::hash_map::Entry::Vacant(entry) = locked.entry(file.id)
                && let Some(store) = store.try_lock(file.id).map_err(sqlx::Error::Io)?
            {
                entry.insert(store);
            }
        }
        if locked.is_empty() {
            continue;
        }
        let ids: Vec<_> = locked.keys().copied().collect();
        // Журнал задачи на месте держит строка `jobs`, как медиа держит `block_media`.
        let rows: Vec<(Uuid,Option<String>,bool)> = sqlx::query_as("select ids.id,a.state,(m.id is not null or exists(select 1 from jobs j where j.log_media_id=ids.id)) from unnest($1::uuid[]) ids(id) left join attachments a on a.id=ids.id left join block_media m on m.id=ids.id")
            .bind(&ids).fetch_all(pool).await?;
        let known: std::collections::HashMap<_, _> = rows
            .into_iter()
            .map(|(id, state, media)| (id, (state, media)))
            .collect();
        for file in batch {
            let Some(store) = locked.get(&file.id) else {
                continue;
            };
            let (state, media) = known.get(&file.id).expect("catalog row");
            let keep = match file.kind {
                FileKind::Staging => matches!(state.as_deref(), Some("created" | "uploading")),
                FileKind::Blob => {
                    *media || matches!(state.as_deref(), Some("created" | "uploading" | "ready"))
                }
            };
            if keep {
                continue;
            }
            let deleted = match file.kind {
                FileKind::Staging => store.delete_staging(file.id).await,
                FileKind::Blob => store.delete_blob(file.id).await,
            };
            match (deleted, file.kind) {
                (Ok(()), FileKind::Staging) => report.orphan_staging += 1,
                (Ok(()), FileKind::Blob) => report.orphan_blobs += 1,
                (Err(error), _) => log_file_error(file.id, &error),
            }
        }
    }
    // После обхода файлов: lock-файл убранного здесь же orphan уходит тем же проходом.
    report.stale_locks = sweep_locks(pool, store, cutoff).await?;
    Ok(report)
}

/// Служба-одиночка `canterlot:attachment-gc`; останавливается вместе с `serve`.
pub async fn run(
    pool: PgPool,
    store: Arc<dyn BlobStore>,
    config: AttachmentsConfig,
    shutdown: watch::Receiver<bool>,
) {
    let interval = config.gc_interval.max(Duration::from_millis(10));
    crate::singleton::supervise(&pool, "attachment-gc", shutdown, || async {
        loop {
            match run_once(&pool, &*store, Utc::now(), config.gc_grace).await {
                Ok(report) if report != GcReport::default() => tracing::info!(
                    kind = "attachment_gc",
                    expired_uploads = report.expired_uploads,
                    released = report.released,
                    orphan_blobs = report.orphan_blobs,
                    orphan_staging = report.orphan_staging,
                    unreferenced_media = report.unreferenced_media,
                    stale_locks = report.stale_locks,
                    "attachment gc pass"
                ),
                Ok(_) => {}
                Err(error) => log_db_error(&error),
            }
            tokio::time::sleep(interval).await;
        }
    })
    .await;
}

fn log_db_error(error: &sqlx::Error) {
    tracing::error!(kind = "attachment_gc_db_error", error = %crate::dberr::describe(error), "attachment gc failed");
}
