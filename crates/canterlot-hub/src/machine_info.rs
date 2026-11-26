use crate::{
    events::{self, NewEvent, Scope},
    session::{Availability, Registry},
};
use canterlot_proto::{
    limits::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_INPUT, MAX_METRICS_EXECUTIONS},
    worker::v1::{MachineMetrics, MachineReport},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
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
/// Имена моделей приходят от CLI. Отказ закрыл бы поток, и воркер прислал бы тот же отчёт снова.
fn replace_nul(report: &mut MachineReport) {
    let mut texts: Vec<&mut String> = vec![
        &mut report.worker_version,
        &mut report.containment,
        &mut report.os,
        &mut report.home_dir,
        &mut report.worktree_root,
    ];
    texts.extend(&mut report.allowed_roots);
    texts.extend(&mut report.memory_work_roots);
    for harness in &mut report.harnesses {
        texts.extend([
            &mut harness.harness,
            &mut harness.provider,
            &mut harness.title,
            &mut harness.installed_version,
            &mut harness.latest_version,
            &mut harness.min_version,
            &mut harness.update_command,
        ]);
        texts.extend(&mut harness.permission_modes);
        for entry in &mut harness.catalog {
            texts.extend([
                &mut entry.name,
                &mut entry.description,
                &mut entry.kind,
                &mut entry.argument_hint,
            ]);
        }
        for model in &mut harness.models {
            texts.extend([&mut model.id, &mut model.title]);
            texts.extend(&mut model.efforts);
        }
    }
    texts.into_iter().for_each(crate::blocks::replace_nul);
}

pub async fn apply_report(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    report: &MachineReport,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    apply_report_with_capabilities(pool, owner_id, machine_id, report, &[], correlation_id)
        .await
        .map(|_| ())
}

/// `negotiated` — необязательные возможности живого соединения: PWA читает их из строки машины.
/// Возвращает разговоры, у которых смена метки харнесса сняла удержания: их очередь надо разбудить.
pub async fn apply_report_with_capabilities(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    report: &MachineReport,
    negotiated: &[String],
    correlation_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut capabilities = vec!["pilot_v1".to_owned()];
    let mut extra: Vec<String> = negotiated
        .iter()
        .filter(|c| *c != "pilot_v1")
        .cloned()
        .collect();
    extra.sort();
    extra.dedup();
    capabilities.extend(extra);
    let mut report = report.clone();
    replace_nul(&mut report);
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
    crate::conversations::lock_owner(&mut *conn, owner_id).await?;
    machine(&mut *conn, owner_id, machine_id).await?;
    let previous: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "select c.id,c.harness,coalesce(h.title,'Неизвестный харнесс'),coalesce(h.provider,'')
         from conversations c left join machine_harnesses h on h.machine_id=c.machine_id and h.harness=c.harness
         where c.machine_id=$1 and c.owner_id=$2 order by c.id",
    ).bind(machine_id).bind(owner_id).fetch_all(&mut *conn).await?;
    sqlx::query("update machines set capabilities=$9,worker_version=$2,allowed_roots=$3,containment=$4,os=$5,reported_at=clock_timestamp(),limits_ok=$6,home_dir=nullif($7,''),worktree_root=nullif($8,''),memory_work_roots=case when 'memory_scope_v1'=any($9) then $10 end,jobs_max_running=case when 'jobs_inplace_v1'=any($9) and $11>0 then $11 end where id=$1")
 .bind(machine_id).bind(&report.worker_version).bind(json!(report.allowed_roots)).bind(&report.containment).bind(&report.os).bind(report.max_attachment_bytes==MAX_ATTACHMENT_BYTES && report.max_attachments_per_input as usize==MAX_ATTACHMENTS_PER_INPUT).bind(&report.home_dir).bind(&report.worktree_root).bind(&capabilities).bind(json!(report.memory_work_roots)).bind(i32::try_from(report.jobs_max_running).unwrap_or(0)).execute(&mut *conn).await?;
    let offered = crate::limits::offered_accounts(&mut *conn, owner_id).await?;
    let harness_state = crate::machine_updates::previous_for_report(&mut *conn, machine_id).await?;
    // Метки до отчёта: сменившаяся метка снимает удержания по прежней учётной записи.
    let previous_accounts: Vec<(String, String, String)> = sqlx::query_as(
        "select harness, provider, account from machine_harnesses where machine_id=$1 order by harness",
    )
    .bind(machine_id)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query("delete from machine_harnesses where machine_id=$1")
        .bind(machine_id)
        .execute(&mut *conn)
        .await?;

    for h in &report.harnesses {
        let models: Vec<_> = h
            .models
            .iter()
            .map(|m| json!({"id":m.id,"title":m.title,"efforts":m.efforts,"images":m.images,"context_window":m.context_window}))
            .collect();
        let catalog: Vec<_> = h
            .catalog
            .iter()
            .map(|e| json!({"name":e.name,"description":e.description,"kind":e.kind,"argument_hint":e.argument_hint}))
            .collect();
        let account = reported_account(&previous_accounts, h);
        sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title,images,settings_midsession,permission_modes,models,settings_midsession_effort,installed_version,latest_version,min_version,update_command,catalog,goals,account) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)")
  .bind(machine_id).bind(&h.harness).bind(&h.provider).bind(&h.title).bind(h.images).bind(h.settings_midsession).bind(json!(h.permission_modes)).bind(json!(models)).bind(h.settings_midsession_effort.unwrap_or(true)).bind(&h.installed_version).bind(&h.latest_version).bind(&h.min_version).bind(&h.update_command).bind(json!(catalog)).bind(h.goals).bind(account).execute(&mut *conn).await?;
    }
    let mut released = Vec::new();
    for (harness, provider, old_account) in &previous_accounts {
        let Some(current) = report.harnesses.iter().find(|h| &h.harness == harness) else {
            continue;
        };
        let new_account = reported_account(&previous_accounts, current);
        if new_account != old_account {
            released.extend(
                crate::crunch::release_relabelled_in_tx(
                    &mut *conn,
                    owner_id,
                    machine_id,
                    harness,
                    provider,
                    old_account,
                    new_account,
                    chrono::Utc::now(),
                    correlation_id,
                )
                .await?,
            );
        }
    }
    let versions_enabled = capabilities
        .iter()
        .any(|capability| capability == crate::session::CAPABILITY_CLI_VERSIONS_V1);
    crate::machine_updates::generate_in_tx(
        &mut *conn,
        owner_id,
        machine_id,
        &harness_state,
        &report,
        versions_enabled,
        correlation_id,
    )
    .await?;
    let mut updates = Vec::new();
    for (id, harness, old_title, old_provider) in previous {
        let current = report.harnesses.iter().find(|h| h.harness == harness);
        let title = current.map_or("Неизвестный харнесс", |h| {
            h.title.as_str()
        });
        let provider = current.map_or("", |h| h.provider.as_str());
        if title != old_title || provider != old_provider {
            let payload = crate::conversations::attribute_payload(&mut *conn, owner_id, id).await?;
            crate::conversations::append(
                &mut *conn,
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
            &mut *conn,
            Scope::Conversations { owner_id },
            "conversation_updated",
            Some(id),
            payload,
            correlation_id,
        )
        .await?;
    }
    crate::limits::emit_offered_changed(&mut *conn, owner_id, &offered, correlation_id).await?;
    emit(&mut *conn,owner_id,"machine_report_updated",json!({"machine_id":machine_id,"worker_version":report.worker_version,"capabilities":capabilities,"limits_ok":report.max_attachment_bytes==MAX_ATTACHMENT_BYTES&&report.max_attachments_per_input as usize==MAX_ATTACHMENTS_PER_INPUT,"home_dir":(!report.home_dir.is_empty()).then_some(&report.home_dir)}),correlation_id).await?;
    Ok(released)
        })
    })
    .await
}
/// Метка харнесса, которую отчёт делает сохранённой. Воркер с учётными записями всегда
/// называет метку явно, в том числе `owner`; пустая метка значит воркер старше учётных
/// записей или откатившийся, и такой отчёт не меняет прежде сохранённую метку (для харнесса,
/// которого ещё не было, это `owner`). Так факты без метки продолжают отбрасываться, пока
/// машина объявляла чужую запись, и не попадают в данные `owner`. Негодная непустая метка
/// читается как `owner`.
fn reported_account<'a>(
    previous: &'a [(String, String, String)],
    harness: &'a canterlot_proto::worker::v1::HarnessInfo,
) -> &'a str {
    if harness.account.is_empty() {
        return previous
            .iter()
            .find(|(name, _, _)| *name == harness.harness)
            .map_or(canterlot_proto::limits::OWNER_ACCOUNT, |(_, _, account)| {
                account.as_str()
            });
    }
    if canterlot_proto::limits::valid_account(&harness.account) {
        harness.account.as_str()
    } else {
        canterlot_proto::limits::OWNER_ACCOUNT
    }
}

/// Машинный `cpu_permille` нормирован на всю машину, поэтому 1000 — его потолок.
/// Битые счётчики исполнений отбрасываются по одному в `sanitize_executions`.
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
    None
}
/// `cpu_permille` исполнения — тысячные доли одного ядра, сумма по его дереву
/// процессов, поэтому многопоточный CLI или сборка даёт больше 1000
/// (2400 = 2,4 ядра). Принимаем все ядра машины и 5% на дрожание замера.
fn execution_cpu_limit(cpu_cores: u32) -> u64 {
    u64::from(cpu_cores.max(1)) * 1000 * 105 / 100
}
/// Сломанный счётчик повторяется в каждой выборке; предупреждаем редко.
const DROPPED_EXECUTIONS_WARN: Duration = Duration::from_secs(300);
fn warn_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= DROPPED_EXECUTIONS_WARN)
}
fn warn_dropped_executions(dropped: usize) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let now = Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if warn_due(*last, now) {
        *last = Some(now);
        tracing::warn!(
            kind = "machine_metrics_execution_dropped",
            reason = "execution_counter",
            dropped,
            "metrics execution dropped"
        );
    }
}
/// Одна битая запись исполнения не повод выбрасывать машину, память, диски и
/// остальные исполнения: отбрасываем только её.
fn sanitize_executions(metrics: &mut MachineMetrics) -> usize {
    let limit = execution_cpu_limit(metrics.cpu_cores);
    let before = metrics.executions.len();
    metrics
        .executions
        .retain(|e| u64::from(e.cpu_permille) <= limit && e.rss_bytes <= i64::MAX as u64);
    before - metrics.executions.len()
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
    apply_metrics_with_registry(pool, owner_id, machine_id, m, None, correlation_id).await
}

/// Окно правдоподобия часов машины без сверенного смещения: выборка, снятая
/// заметно раньше приёма или в будущем, не доказывает свежесть условия.
const SAMPLE_TIME_MAX_AGE: TimeDelta = TimeDelta::hours(24);
const SAMPLE_TIME_MAX_AHEAD: TimeDelta = TimeDelta::minutes(5);
const METRICS_STALE_AGE: TimeDelta = TimeDelta::seconds(91);

/// Время выборки в часах хаба: смещение сеанса, если оно известно, иначе
/// собственные часы выборки в пределах правдоподобия. Нулевая или нелепая
/// отметка не должна доказывать условие: такая выборка считается устаревшей.
pub(crate) fn sample_observed_at(
    registry: Option<&Registry>,
    machine_id: Uuid,
    machine_ms: u64,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    if machine_ms == 0 {
        return now;
    }
    let mapped = registry
        .and_then(|registry| registry.to_hub_time(machine_id, machine_ms))
        .or_else(|| {
            i64::try_from(machine_ms)
                .ok()
                .and_then(DateTime::from_timestamp_millis)
        });
    match mapped {
        Some(at) if at >= now - SAMPLE_TIME_MAX_AGE && at <= now + SAMPLE_TIME_MAX_AHEAD => at,
        _ => now - METRICS_STALE_AGE,
    }
}

pub(crate) async fn apply_metrics_with_registry(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    m: &MachineMetrics,
    registry: Option<&Registry>,
    correlation_id: Uuid,
) -> Result<MetricsOutcome, sqlx::Error> {
    if let Some(reason) = validate(m) {
        return Ok(ignored(reason));
    }
    let m = {
        let mut metrics = m.clone();
        let dropped = sanitize_executions(&mut metrics);
        if dropped > 0 {
            warn_dropped_executions(dropped);
        }
        metrics
    };
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
    let now = Utc::now();
    // Свежесть условия считается по времени выборки, а не по времени приёма:
    // задержанный отчёт не должен срабатывать старыми показаниями.
    let observed_at = sample_observed_at(registry, machine_id, m.machine_time_ms, now);
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
    machine(&mut *conn, owner_id, machine_id).await?;
    let owned:i64=sqlx::query_scalar("select count(*) from executions e join conversations c on c.id=e.conversation_id where e.id=any($1) and c.machine_id=$2 and c.owner_id=$3").bind(&ids).bind(machine_id).bind(owner_id).fetch_one(&mut *conn).await?;
    if owned as usize != ids.len() {
        return Ok(ignored("foreign_execution"));
    }
    let disks: Vec<_> = m
        .disks
        .iter()
        .map(|d| json!({"label":d.label,"total_bytes":d.total_bytes,"free_bytes":d.free_bytes}))
        .collect();
    let executions:Vec<_>=m.executions.iter().map(|e| {
        let mut row = json!({"execution_id":e.execution_id,"alive":e.alive,"cpu_permille":e.cpu_permille,"rss_bytes":e.rss_bytes});
        if let Some(valid) = e.cpu_valid {
            row["cpu_valid"] = json!(valid);
        }
        if let Some(alive) = e.cli_alive {
            row["cli_alive"] = json!(alive);
        }
        if let Some(descendants) = e.descendants {
            row["descendants"] = json!(descendants);
        }
        row
    }).collect();
    // Свежесть стража и будильников — время самой выборки, а не приёма:
    // задержанный отчёт не должен выглядеть свежим.
    let stored=sqlx::query("insert into machine_metrics(machine_id,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions,observed_at) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) on conflict(machine_id) do update set observed_at=excluded.observed_at,machine_time_ms=excluded.machine_time_ms,cpu_cores=excluded.cpu_cores,cpu_permille=excluded.cpu_permille,mem_total_bytes=excluded.mem_total_bytes,mem_available_bytes=excluded.mem_available_bytes,load1_centi=excluded.load1_centi,load5_centi=excluded.load5_centi,load15_centi=excluded.load15_centi,disks=excluded.disks,executions=excluded.executions where machine_metrics.machine_time_ms<excluded.machine_time_ms")
 .bind(machine_id).bind(m.machine_time_ms as i64).bind(m.cpu_cores as i32).bind(m.cpu_permille as i32).bind(m.mem_total_bytes as i64).bind(m.mem_available_bytes as i64).bind(m.load1_centi as i32).bind(m.load5_centi as i32).bind(m.load15_centi as i32).bind(json!(disks)).bind(json!(executions)).bind(observed_at).execute(&mut *conn).await?.rows_affected();
    if stored == 0 {
        return Ok(ignored("stale_sample"));
    }
    // Занятость после хода считается по настоящему id исполнения: две выборки подряд
    // выше порога; пропуск в отчёте сбрасывает серию.
    for e in &m.executions {
        let Ok(id) = Uuid::parse_str(&e.execution_id) else {
            continue;
        };
        let busy = e.alive && e.cpu_permille >= 150;
        if busy {
            sqlx::query(
                "update executions set cpu_busy_streak=cpu_busy_streak+1,
                     cpu_busy_first_at=coalesce(cpu_busy_first_at,$2)
                 where id=$1",
            )
            .bind(id)
            .bind(observed_at)
            .execute(&mut *conn)
            .await?;
        } else {
            sqlx::query(
                "update executions set cpu_busy_streak=0,cpu_busy_first_at=null where id=$1",
            )
            .bind(id)
            .execute(&mut *conn)
            .await?;
        }
    }
    sqlx::query(
        "update executions e set cpu_busy_streak=0,cpu_busy_first_at=null
         from conversations c
         where c.id=e.conversation_id and c.machine_id=$1 and e.id<>all($2)
           and (e.cpu_busy_streak<>0 or e.cpu_busy_first_at is not null)",
    )
    .bind(machine_id)
    .bind(&ids)
    .execute(&mut *conn)
    .await?;
    emit(&mut *conn,owner_id,"machine_metrics",json!({"machine_id":machine_id,"machine_time_ms":m.machine_time_ms,"cpu_cores":m.cpu_cores,"cpu_permille":m.cpu_permille,"mem_total_bytes":m.mem_total_bytes,"mem_available_bytes":m.mem_available_bytes,"load1_centi":m.load1_centi,"load5_centi":m.load5_centi,"load15_centi":m.load15_centi,"disks":disks,"executions":executions}),correlation_id).await?;
    // Сторож машины проверяет взведённые будильники этой машины на каждом свежем отчёте.
    // Выборка могла быть снята раньше приёма: оба условия сверяются с её временем.
    crate::session_tools::alarms::evaluate_machine_in_tx(
        &mut *conn,
        machine_id,
        &m,
        observed_at,
        now,
        correlation_id,
    )
    .await?;
    Ok(MetricsOutcome::Stored)
        })
    })
    .await
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
    let registry = registry.cloned();
    // Решение о доступности принимается по реестру до транзакции: снимок очереди,
    // принятый позже (быстрое переподключение), очистка не трогает.
    let decided_at = Utc::now();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let rows: Vec<(Uuid, Uuid, String, String, Option<String>)> = sqlx::query_as(
                "select id,owner_id,availability,role,runner_availability from machines
                 order by id for no key update",
            )
            .fetch_all(&mut *conn)
            .await?;
            let mut changes = Vec::new();
            // Раннер сообщает клиентам свой словарь (online, offline, unknown) с возрастом
            // наблюдения; прежняя колонка `availability` у него по-прежнему идёт по сеансу
            // воркера (аренды, простои), но отдельного события не даёт.
            let mut runner_changes = Vec::new();
            for (id, owner, old, role, old_runner) in rows {
                let new = registry
                    .as_ref()
                    .map_or("unavailable", |r| availability(r.availability(id)));
                if old != new {
                    sqlx::query(
                "update machines set availability=$2,availability_at=clock_timestamp() where id=$1",
            )
            .bind(id)
            .bind(new)
            .execute(&mut *conn)
            .await?;
                    changes.push((owner, id, new, role == crate::machines::ROLE_RUNNER));
                }
                if role == crate::machines::ROLE_RUNNER {
                    // Первый разбор после старта хаба реестра не имеет: доступность неизвестна.
                    let (state, observed_at) = match registry.as_ref() {
                        Some(registry) => {
                            crate::machines::runner_availability_of(
                                &mut *conn,
                                registry,
                                id,
                                Utc::now(),
                            )
                            .await?
                        }
                        None => (crate::machines::RunnerAvailability::Unknown, None),
                    };
                    if old_runner.as_deref() != Some(state.as_str()) {
                        // Время хаба, с которого раннер `offline`; любое другое состояние
                        // его снимает. Правило этапа C отсчитывает от него десять минут.
                        sqlx::query(
                            "update machines set runner_availability=$2,
                                    runner_offline_since=case when $2='offline'
                                        then clock_timestamp() end
                             where id=$1",
                        )
                        .bind(id)
                        .bind(state.as_str())
                        .execute(&mut *conn)
                        .await?;
                        runner_changes.push((owner, id, state.as_str(), observed_at));
                    }
                }
            }
            changes.sort_unstable();
            runner_changes.sort_unstable_by_key(|change| (change.0, change.1));
            for &(owner, id, new, is_runner) in &changes {
                if new == "unavailable" {
                    crate::leases::clear_in_tx(&mut *conn, owner, id, decided_at, correlation_id)
                        .await?;
                }
                if !is_runner {
                    emit(
                        &mut *conn,
                        owner,
                        "machine_availability_changed",
                        json!({"machine_id":id,"availability":new}),
                        correlation_id,
                    )
                    .await?;
                }
            }
            for &(owner, id, state, observed_at) in &runner_changes {
                let mut payload = json!({"machine_id":id,"availability":state});
                if let Some(at) = observed_at {
                    payload["observed_at"] =
                        json!(at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                }
                emit(
                    &mut *conn,
                    owner,
                    "machine_availability_changed",
                    payload,
                    correlation_id,
                )
                .await?;
            }
            Ok(changes.len() + runner_changes.len())
        })
    })
    .await
}

pub async fn run_status_projection(
    pool: PgPool,
    registry: Registry,
    shutdown: watch::Receiver<bool>,
) {
    run_status_projection_with(pool, registry, Default::default(), shutdown).await
}

/// Сроки проекции доступности машин. Боевые значения дают `Default`; тесты сжимают
/// их, не меняя формы: пауза захвата так же растёт вдвое.
#[derive(Clone, Copy, Debug)]
pub struct StatusProjectionTimers {
    /// Пауза между захватами замка после сбоя.
    pub backoff: crate::singleton::Backoff,
    /// Плановый пересчёт доступности помимо изменений сеансов.
    pub tick: std::time::Duration,
}

impl Default for StatusProjectionTimers {
    fn default() -> Self {
        Self {
            backoff: Default::default(),
            tick: std::time::Duration::from_secs(5),
        }
    }
}

pub async fn run_status_projection_with(
    pool: PgPool,
    registry: Registry,
    timers: StatusProjectionTimers,
    shutdown: watch::Receiver<bool>,
) {
    let first_acquire = std::sync::atomic::AtomicBool::new(true);
    crate::singleton::supervise_with(
        &pool,
        "machine_status",
        timers.backoff,
        shutdown,
        || async {
            let first = first_acquire.swap(false, std::sync::atomic::Ordering::Relaxed);
            if project(
                &pool,
                if first { None } else { Some(&registry) },
                Uuid::now_v7(),
            )
            .await
            .is_err()
            {
                tracing::warn!(
                    kind = "machine_status_database",
                    "initial projection failed"
                );
                return;
            }
            let mut changes = registry.subscribe_changes();
            let mut tick = tokio::time::interval(timers.tick);
            loop {
                tokio::select! {_=tick.tick()=>{},_=changes.recv()=>{}}
                if project_availability(&pool, &registry, Uuid::now_v7())
                    .await
                    .is_err()
                {
                    tracing::warn!(kind = "machine_status_database", "projection failed");
                }
            }
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn dropped_execution_warning_is_rate_limited() {
        let now = Instant::now();
        assert!(warn_due(None, now), "первое предупреждение проходит");
        assert!(
            !warn_due(Some(now), now + Duration::from_secs(1)),
            "следующая выборка не повторяет предупреждение"
        );
        assert!(warn_due(Some(now), now + DROPPED_EXECUTIONS_WARN));
    }

    #[test]
    fn execution_cpu_limit_allows_all_cores_with_jitter() {
        assert_eq!(execution_cpu_limit(0), 1050);
        assert_eq!(execution_cpu_limit(36), 37800);
    }
}
