//! Сборщик вложений: просроченные загрузки, неиспользованные готовые вложения и orphan-файлы.
//! Каждое решение берёт строку `for update skip locked` и перепроверяет условия после блокировки.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use super::{BlobStore, FileKind, linked, state_event};
use crate::{config::AttachmentsConfig, singleton::Singleton};

const BATCH: i64 = 1000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GcReport {
    pub expired_uploads: usize,
    pub released: usize,
    pub orphan_blobs: usize,
    pub orphan_staging: usize,
    /// Опубликованные картинки агента, на которые так и не сослался блок.
    pub unreferenced_media: usize,
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
        return Ok(false);
    };
    let store = &*locked;
    let mut tx = pool.begin().await?;
    let row: Option<(String, DateTime<Utc>, Uuid)> = sqlx::query_as(
        "select state, updated_at, conversation_id from attachments where id=$1 for update skip locked",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((state, updated_at, conversation_id)) = row else {
        return Ok(false);
    };
    if !matches!(state.as_str(), "created" | "uploading") || updated_at >= cutoff {
        return Ok(false);
    }
    sqlx::query(
        "update attachments set state='failed', fail_reason='upload_expired', updated_at=clock_timestamp() where id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    state_event(
        &mut tx,
        conversation_id,
        id,
        "failed",
        Some("upload_expired"),
        Uuid::now_v7(),
    )
    .await?;
    tx.commit().await?;
    if let Err(error) = store.delete_staging(id).await {
        log_file_error(id, &error);
    }
    Ok(true)
}

/// Готовое вложение без ссылок становится `released`; blob удаляется только после commit.
async fn release(
    pool: &PgPool,
    store: &dyn BlobStore,
    id: Uuid,
    cutoff: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Some(locked) = store.try_lock(id).map_err(sqlx::Error::Io)? else {
        return Ok(false);
    };
    let store = &*locked;
    let mut tx = pool.begin().await?;
    let row: Option<(String, DateTime<Utc>, Uuid)> = sqlx::query_as(
        "select state, updated_at, conversation_id from attachments where id=$1 for update skip locked",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((state, updated_at, conversation_id)) = row else {
        return Ok(false);
    };
    // Новый оператор после блокировки видит ссылку, зафиксированную конкурентной привязкой.
    if state != "ready" || updated_at >= cutoff || linked(&mut tx, id).await? {
        return Ok(false);
    }
    sqlx::query(
        "update attachments set state='released', updated_at=clock_timestamp() where id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    state_event(
        &mut tx,
        conversation_id,
        id,
        "released",
        None,
        Uuid::now_v7(),
    )
    .await?;
    tx.commit().await?;
    if let Err(error) = store.delete_blob(id).await {
        log_file_error(id, &error);
    }
    Ok(true)
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
        return Ok(false);
    };
    let store = &*locked;
    let mut tx = pool.begin().await?;
    let locked: Option<Uuid> = sqlx::query_scalar(
        "select id from block_media where id=$1 and attached_at is null and created_at < $2
         for update skip locked",
    )
    .bind(id)
    .bind(cutoff)
    .fetch_optional(&mut *tx)
    .await?;
    if locked.is_none() {
        return Ok(false);
    }
    sqlx::query("delete from block_media where id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if let Err(error) = store.delete_blob(id).await {
        log_file_error(id, &error);
    }
    Ok(true)
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
    let unused: Vec<Uuid> = sqlx::query_scalar(
        "select a.id from attachments a where a.state='ready' and a.updated_at < $1
           and not exists(select 1 from input_attachments ia where ia.attachment_id=a.id)
           and not exists(select 1 from scheduled_job_attachments sa where sa.attachment_id=a.id)
         order by a.id limit $2",
    )
    .bind(cutoff)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for id in unused {
        if release(pool, store, id, cutoff).await? {
            report.released += 1;
        }
    }
    let unreferenced: Vec<Uuid> = sqlx::query_scalar(
        "select id from block_media where attached_at is null and created_at < $1 order by id limit $2",
    )
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
    for file in files.into_iter().filter(|f| f.mtime < cutoff) {
        let Some(locked) = store.try_lock(file.id).map_err(sqlx::Error::Io)? else {
            continue;
        };
        let store = &*locked;
        let state: Option<String> = sqlx::query_scalar("select state from attachments where id=$1")
            .bind(file.id)
            .fetch_optional(pool)
            .await?;
        let state = state.as_deref();
        // Blob картинки агента живёт, пока есть её строка; staging приёма короткоживущий.
        let media: bool = match file.kind {
            FileKind::Blob => {
                sqlx::query_scalar("select exists(select 1 from block_media where id=$1)")
                    .bind(file.id)
                    .fetch_one(pool)
                    .await?
            }
            FileKind::Staging => false,
        };
        let keep = media
            || match file.kind {
                FileKind::Staging => matches!(state, Some("created" | "uploading")),
                FileKind::Blob => matches!(state, Some("created" | "uploading" | "ready")),
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
    Ok(report)
}

/// Служба-одиночка `canterlot:attachment-gc`; останавливается вместе с `serve`.
pub async fn run(
    pool: PgPool,
    store: Arc<dyn BlobStore>,
    config: AttachmentsConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    let options = pool.connect_options();
    let interval = config.gc_interval.max(Duration::from_millis(10));
    let mut singleton = loop {
        let acquired = tokio::select! {biased; _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>return, result=Singleton::acquire(&options,"attachment-gc")=>result};
        match acquired {
            Ok(Some(lock)) => break lock,
            Err(error) => log_db_error(&error),
            _ => {}
        }
        tokio::select! {biased; _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>return, _=tokio::time::sleep(interval)=>{}}
    };
    let work = async {
        loop {
            if !singleton.still_held().await {
                break;
            }
            match run_once(&pool, &*store, Utc::now(), config.gc_grace).await {
                Ok(report) if report != GcReport::default() => tracing::info!(
                    kind = "attachment_gc",
                    expired_uploads = report.expired_uploads,
                    released = report.released,
                    orphan_blobs = report.orphan_blobs,
                    orphan_staging = report.orphan_staging,
                    unreferenced_media = report.unreferenced_media,
                    "attachment gc pass"
                ),
                Ok(_) => {}
                Err(error) => log_db_error(&error),
            }
            tokio::time::sleep(interval).await;
        }
    };
    tokio::select! {biased; _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>{}, _=work=>{}}
    singleton.release().await;
}

fn log_db_error(error: &sqlx::Error) {
    tracing::error!(kind = "attachment_gc_db_error", error = %crate::dberr::describe(error), "attachment gc failed");
}
