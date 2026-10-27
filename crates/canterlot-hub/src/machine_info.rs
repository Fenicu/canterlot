use crate::{
    events::{self, NewEvent, Scope},
    session::{Availability, Registry},
    singleton::Singleton,
};
use canterlot_proto::{
    limits::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_INPUT, MAX_METRICS_EXECUTIONS},
    worker::v1::{MachineMetrics, MachineReport},
};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsOutcome {
    Stored,
    Ignored(&'static str),
}
async fn machine(conn: &mut PgConnection, owner: Uuid, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select id from machines where id=$1 and owner_id=$2 for no key update")
        .bind(id)
        .bind(owner)
        .fetch_one(conn)
        .await?;
    Ok(())
}
pub async fn apply_report(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    report: &MachineReport,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    crate::conversations::lock_owner(&mut tx, owner_id).await?;
    machine(&mut tx, owner_id, machine_id).await?;
    let previous: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "select c.id,c.harness,coalesce(h.title,'Неизвестный харнесс'),coalesce(h.provider,'')
         from conversations c left join machine_harnesses h on h.machine_id=c.machine_id and h.harness=c.harness
         where c.machine_id=$1 and c.owner_id=$2 order by c.id",
    ).bind(machine_id).bind(owner_id).fetch_all(&mut *tx).await?;
    sqlx::query("update machines set capabilities=array['pilot_v1'],worker_version=$2,allowed_roots=$3,containment=$4,os=$5,reported_at=clock_timestamp(),limits_ok=$6 where id=$1")
 .bind(machine_id).bind(&report.worker_version).bind(json!(report.allowed_roots)).bind(&report.containment).bind(&report.os).bind(report.max_attachment_bytes==MAX_ATTACHMENT_BYTES && report.max_attachments_per_input as usize==MAX_ATTACHMENTS_PER_INPUT).execute(&mut *tx).await?;
    sqlx::query("delete from machine_harnesses where machine_id=$1")
        .bind(machine_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from machine_profiles where machine_id=$1")
        .bind(machine_id)
        .execute(&mut *tx)
        .await?;
    for h in &report.harnesses {
        let models: Vec<_> = h
            .models
            .iter()
            .map(|m| json!({"id":m.id,"title":m.title,"efforts":m.efforts}))
            .collect();
        sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title,images,settings_midsession,permission_modes,models) values($1,$2,$3,$4,$5,$6,$7,$8)")
  .bind(machine_id).bind(&h.harness).bind(&h.provider).bind(&h.title).bind(h.images).bind(h.settings_midsession).bind(json!(h.permission_modes)).bind(json!(models)).execute(&mut *tx).await?;
    }
    for p in &report.profiles {
        sqlx::query("insert into machine_profiles(machine_id,name,title,description,harnesses) values($1,$2,$3,$4,$5)").bind(machine_id).bind(&p.name).bind(&p.title).bind(&p.description).bind(json!(p.harnesses)).execute(&mut *tx).await?;
    }
    let mut updates = Vec::new();
    for (id, harness, old_title, old_provider) in previous {
        let current = report.harnesses.iter().find(|h| h.harness == harness);
        let title = current.map_or("Неизвестный харнесс", |h| {
            h.title.as_str()
        });
        let provider = current.map_or("", |h| h.provider.as_str());
        if title != old_title || provider != old_provider {
            let payload = crate::conversations::attribute_payload(&mut tx, owner_id, id).await?;
            crate::conversations::append(
                &mut tx,
                Scope::Conversation { id },
                "conversation_updated",
                Some(id),
                payload.clone(),
                correlation_id,
            )
            .await?;
            updates.push((id, payload));
        }
    }
    for (id, payload) in updates {
        crate::conversations::append(
            &mut tx,
            Scope::Conversations { owner_id },
            "conversation_updated",
            Some(id),
            payload,
            correlation_id,
        )
        .await?;
    }
    emit(&mut tx,owner_id,"machine_report_updated",json!({"machine_id":machine_id,"worker_version":report.worker_version,"capabilities":["pilot_v1"],"limits_ok":report.max_attachment_bytes==MAX_ATTACHMENT_BYTES&&report.max_attachments_per_input as usize==MAX_ATTACHMENTS_PER_INPUT}),correlation_id).await?;
    tx.commit().await
}
fn validate(metrics: &MachineMetrics) -> Option<&'static str> {
    if metrics.cpu_permille > 1000 {
        return Some("cpu_permille");
    }
    if metrics.executions.len() > MAX_METRICS_EXECUTIONS {
        return Some("executions_bound");
    }
    if metrics.mem_available_bytes > metrics.mem_total_bytes {
        return Some("memory_range");
    }
    for d in &metrics.disks {
        if d.label != "data"
            && !d.label.strip_prefix("root_").is_some_and(|n| {
                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u32>().is_ok()
            })
        {
            return Some("disk_label");
        }
        if d.free_bytes > d.total_bytes {
            return Some("disk_range");
        }
    }
    if [
        metrics.machine_time_ms,
        metrics.mem_total_bytes,
        metrics.mem_available_bytes,
    ]
    .into_iter()
    .any(|n| n > i64::MAX as u64)
        || [
            metrics.cpu_cores,
            metrics.load1_centi,
            metrics.load5_centi,
            metrics.load15_centi,
        ]
        .into_iter()
        .any(|n| n > i32::MAX as u32)
    {
        return Some("counter_overflow");
    }
    if metrics
        .executions
        .iter()
        .any(|e| e.cpu_permille > 1000 || e.rss_bytes > i64::MAX as u64)
    {
        return Some("execution_counter");
    }
    None
}
fn ignored(reason: &'static str) -> MetricsOutcome {
    tracing::warn!(kind = "machine_metrics_ignored", reason, "metrics ignored");
    MetricsOutcome::Ignored(reason)
}
pub async fn apply_metrics(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    m: &MachineMetrics,
    correlation_id: Uuid,
) -> Result<MetricsOutcome, sqlx::Error> {
    if let Some(reason) = validate(m) {
        return Ok(ignored(reason));
    }
    let mut ids = Vec::new();
    for e in &m.executions {
        let Ok(id) = Uuid::parse_str(&e.execution_id) else {
            return Ok(ignored("execution_id"));
        };
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != m.executions.len() {
        return Ok(ignored("duplicate_execution"));
    }
    let mut tx = pool.begin().await?;
    machine(&mut tx, owner_id, machine_id).await?;
    let owned:i64=sqlx::query_scalar("select count(*) from executions e join conversations c on c.id=e.conversation_id where e.id=any($1) and c.machine_id=$2 and c.owner_id=$3").bind(&ids).bind(machine_id).bind(owner_id).fetch_one(&mut *tx).await?;
    if owned as usize != ids.len() {
        return Ok(ignored("foreign_execution"));
    }
    let disks: Vec<_> = m
        .disks
        .iter()
        .map(|d| json!({"label":d.label,"total_bytes":d.total_bytes,"free_bytes":d.free_bytes}))
        .collect();
    let executions:Vec<_>=m.executions.iter().map(|e|json!({"execution_id":e.execution_id,"alive":e.alive,"cpu_permille":e.cpu_permille,"rss_bytes":e.rss_bytes})).collect();
    let stored=sqlx::query("insert into machine_metrics(machine_id,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) on conflict(machine_id) do update set observed_at=clock_timestamp(),machine_time_ms=excluded.machine_time_ms,cpu_cores=excluded.cpu_cores,cpu_permille=excluded.cpu_permille,mem_total_bytes=excluded.mem_total_bytes,mem_available_bytes=excluded.mem_available_bytes,load1_centi=excluded.load1_centi,load5_centi=excluded.load5_centi,load15_centi=excluded.load15_centi,disks=excluded.disks,executions=excluded.executions where machine_metrics.machine_time_ms<excluded.machine_time_ms")
 .bind(machine_id).bind(m.machine_time_ms as i64).bind(m.cpu_cores as i32).bind(m.cpu_permille as i32).bind(m.mem_total_bytes as i64).bind(m.mem_available_bytes as i64).bind(m.load1_centi as i32).bind(m.load5_centi as i32).bind(m.load15_centi as i32).bind(json!(disks)).bind(json!(executions)).execute(&mut *tx).await?.rows_affected();
    if stored == 0 {
        return Ok(ignored("stale_sample"));
    }
    emit(&mut tx,owner_id,"machine_metrics",json!({"machine_id":machine_id,"machine_time_ms":m.machine_time_ms,"cpu_cores":m.cpu_cores,"cpu_permille":m.cpu_permille,"mem_total_bytes":m.mem_total_bytes,"mem_available_bytes":m.mem_available_bytes,"load1_centi":m.load1_centi,"load5_centi":m.load5_centi,"load15_centi":m.load15_centi,"disks":disks,"executions":executions}),correlation_id).await?;
    tx.commit().await?;
    Ok(MetricsOutcome::Stored)
}
async fn emit(
    conn: &mut PgConnection,
    owner_id: Uuid,
    kind: &'static str,
    payload: serde_json::Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Machines { owner_id },
            kind,
            conversation_id: None,
            execution_id: None,
            payload,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}
fn availability(a: Availability) -> &'static str {
    match a {
        Availability::Available => "available",
        Availability::Degraded => "degraded",
        Availability::Unavailable => "unavailable",
    }
}
pub async fn project_availability(
    pool: &PgPool,
    registry: &Registry,
    correlation_id: Uuid,
) -> Result<usize, sqlx::Error> {
    project(pool, Some(registry), correlation_id).await
}
async fn project(
    pool: &PgPool,
    registry: Option<&Registry>,
    correlation_id: Uuid,
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "select id,owner_id,availability from machines order by id for no key update",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut changes = Vec::new();
    for (id, owner, old) in rows {
        let new = registry.map_or("unavailable", |r| availability(r.availability(id)));
        if old != new {
            sqlx::query(
                "update machines set availability=$2,availability_at=clock_timestamp() where id=$1",
            )
            .bind(id)
            .bind(new)
            .execute(&mut *tx)
            .await?;
            changes.push((owner, id, new));
        }
    }
    changes.sort_unstable();
    for &(owner, id, new) in &changes {
        emit(
            &mut tx,
            owner,
            "machine_availability_changed",
            json!({"machine_id":id,"availability":new}),
            correlation_id,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(changes.len())
}
const PASSIVE_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

pub async fn run_status_projection(
    pool: PgPool,
    registry: Registry,
    mut shutdown: watch::Receiver<bool>,
) {
    let options = pool.connect_options();
    // Второй процесс остаётся пассивным и забирает блокировку, когда её отпустит первый.
    let mut lock = loop {
        if *shutdown.borrow() {
            return;
        }
        let acquired = tokio::select! {biased;
            _ = crate::dispatcher::wait_for_shutdown(&mut shutdown) => return,
            result = Singleton::acquire(&options, "machine_status") => result,
        };
        match acquired {
            Ok(Some(lock)) => break lock,
            Ok(None) => {}
            Err(_) => tracing::warn!(kind = "machine_status_database", "singleton unavailable"),
        }
        tokio::select! {biased;
            _ = crate::dispatcher::wait_for_shutdown(&mut shutdown) => return,
            _ = tokio::time::sleep(PASSIVE_RETRY) => {}
        }
    };
    if !lock.still_held().await {
        return;
    }
    let reset = tokio::select! {biased;_=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>{lock.release().await;return;},result=project(&pool,None,Uuid::now_v7())=>result};
    if reset.is_err() {
        lock.release().await;
        return;
    }
    let mut changes = registry.subscribe_changes();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        tokio::select! {biased;_=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>break,_=tick.tick()=>{},_=changes.recv()=>{}}
        if !lock.still_held().await {
            break;
        }
        let result = tokio::select! {biased;_=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>break,result=project_availability(&pool,&registry,Uuid::now_v7())=>result};
        if result.is_err() {
            tracing::warn!(kind = "machine_status_database", "projection failed");
        }
    }
    lock.release().await;
}
