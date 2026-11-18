//! Приём заявленного запуска: проверки, предпроверка и dispatch вне тика.

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    config::LimitsConfig,
    conversations::{self, CrunchSetting, NewConversation, SubmitMessage, submit_message_in_tx},
    delivery::ContentBlock,
    events::{self, NewEvent, Scope},
    http::AppState,
    limit_settings, limits,
    model::HoldReason,
    scheduler::Scheduler,
    session::{CAPABILITY_SCHEDULE_PRECHECK_V1, PrecheckFailure},
    session_tools::routing::{self, RouteError, RouteInputs, RouteRequest},
};

use super::{
    ClaimedRun, OverlapPolicy, RunState, Schedule, ScheduleError, ScheduleRun, ScheduleTarget,
    delay_ms, render_title, set_run_state,
};

#[derive(Debug)]
struct Skip {
    state: RunState,
    detail: Value,
}
impl Skip {
    fn new(state: RunState, reason: &str) -> Self {
        Self {
            state,
            detail: json!({"reason": reason}),
        }
    }
    fn with(state: RunState, detail: Value) -> Self {
        Self { state, detail }
    }
}

type AdmitResult = Result<(Uuid, Uuid, Value), Skip>;

/// Обработка одного `claimed`-запуска. Все проверки идут вне транзакции тика.
pub async fn admit(scheduler: &Scheduler, claimed: ClaimedRun) {
    let run = claimed.run;
    let schedule = claimed.schedule;
    let result = admit_inner(scheduler, &schedule, &run, false, false).await;
    let (state, detail, session_id, input_id, started_at, delay) = match result {
        Ok((session_id, input_id, dispatch_detail)) => {
            let started_at = Utc::now();
            let mut detail = run.detail.clone();
            merge(&mut detail, dispatch_detail);
            (
                RunState::Started,
                detail,
                Some(session_id),
                Some(input_id),
                Some(started_at),
                Some(delay_ms(run.scheduled_at, started_at)),
            )
        }
        Err(skip) => {
            let mut detail = run.detail.clone();
            merge(&mut detail, skip.detail);
            (skip.state, detail, None, None, None, None)
        }
    };
    // Итог и его событие — одна транзакция: наблюдатель никогда не видит
    // состояние без события и событие без состояния.
    let mut tx = match crate::db::begin(scheduler.pool()).await {
        Ok(tx) => tx,
        Err(error) => {
            tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
            return;
        }
    };
    if let Err(error) = set_run_state(
        &mut tx, run.id, state, detail.clone(), started_at, delay, session_id, input_id,
    )
    .await
    {
        let _ = tx.rollback().await;
        if matches!(error, sqlx::Error::RowNotFound) {
            // Расписание удалили, пока шёл приём: строка запуска ушла
            // каскадом, событие о призраке не публикуем.
            return;
        }
        tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
        return;
    }
    let updated = apply_outcome(&run, state, detail.clone(), session_id, input_id, started_at, delay);
    let payload = match serde_json::to_value(&updated) {
        Ok(payload) => payload,
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::error!(kind = "schedule_admit_db", error = %error, "cannot serialize run outcome");
            return;
        }
    };
    if let Err(error) = events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Schedules {
                owner_id: schedule.owner_id,
            },
            kind: "schedule_run_changed",
            conversation_id: None,
            execution_id: None,
            payload,
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot publish run outcome");
        return;
    }
    if state == RunState::Failed {
        let reason = detail.get("reason").and_then(Value::as_str).unwrap_or("failed");
        if let Err(error) = crate::notify::enqueue_schedule_failure_in_tx(
            &mut tx,
            schedule.id,
            &schedule.name,
            reason,
        )
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot queue failure push");
            return;
        }
    }
    if let Err(error) = tx.commit().await {
        tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot commit run outcome");
    }
}

/// Итоговая строка запуска: база из заявки плюс исход приёма.
#[allow(clippy::too_many_arguments)]
fn apply_outcome(
    run: &ScheduleRun,
    state: RunState,
    detail: Value,
    session_id: Option<Uuid>,
    input_id: Option<Uuid>,
    started_at: Option<DateTime<Utc>>,
    delay: Option<i64>,
) -> ScheduleRun {
    ScheduleRun {
        id: run.id,
        schedule_id: run.schedule_id,
        scheduled_at: run.scheduled_at,
        claimed_revision: run.claimed_revision,
        state,
        created_at: run.created_at,
        started_at,
        delay_ms: delay,
        session_id,
        input_id,
        manual: run.manual,
        idempotency_key: run.idempotency_key.clone(),
        detail,
    }
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(object), Some(extra)) = (target.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
}

/// Более ранние запуски, блокирующие данный при политике `skip`:
/// `claimed/prechecking` или `started`, чей ввод ещё не принят
/// (`queued/dispatched/on_machine/transmitting`) или чья сессия
/// работает/ждёт владельца. Поздние заявки ранних не блокируют.
async fn earlier_blocking(
    conn: &mut PgConnection,
    schedule_id: Uuid,
    run_id: Uuid,
) -> Result<i64, Skip> {
    sqlx::query_scalar(
        "select count(*) from schedule_runs r
         left join inputs i on i.id = r.input_id
         left join conversation_derived d on d.conversation_id = r.session_id
         where r.schedule_id = $1
           and r.scheduled_at < (select scheduled_at from schedule_runs where id = $2)
           and (r.state in ('claimed','prechecking')
                or (r.state = 'started'
                    and (i.state in ('queued','dispatched','on_machine','transmitting')
                         or d.activity_state in ('working','waiting_for_user','stalled'))))",
    )
    .bind(schedule_id)
    .bind(run_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
}

async fn admit_inner(
    scheduler: &Scheduler,
    schedule: &Schedule,
    run: &ScheduleRun,
    run_anyway: bool,
    manual: bool,
) -> AdmitResult {
    // Граница параллельных приёмов: каждый приём держит соединение лишь
    // короткими участками, но предпроверка ждёт воркер до минуты.
    // Без семафора 32 заявленных тиком приёма занимают весь пул.
    static ADMIT_SEMAPHORE: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    let semaphore = ADMIT_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(8));
    let _permit = semaphore
        .acquire()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    if !run_anyway && !manual && schedule.overlap == OverlapPolicy::Skip {
        let mut conn = scheduler
            .pool()
            .acquire()
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
        if earlier_blocking(&mut conn, schedule.id, run.id).await? > 0 {
            return Err(Skip::new(RunState::SkippedOverlap, "earlier_run_in_flight"));
        }
    }

    let (machine_id, existing_conversation, existing_workdir) = match &schedule.target {
        ScheduleTarget::NewSession { machine_id, .. } => (*machine_id, None, None),
        ScheduleTarget::ExistingSession { conversation_id } => {
            let mut conn = scheduler
                .pool()
                .acquire()
                .await
                .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
            let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
                "select c.machine_id,q.conversation_id,c.workdir from conversations c
                 join queue_state q on q.conversation_id=c.id
                 where c.id=$1 and c.owner_id=$2 and c.trashed_at is null",
            )
            .bind(conversation_id)
            .bind(schedule.owner_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
            let Some((machine_id, conversation_id, workdir)) = row else {
                return Err(Skip::new(RunState::Failed, "conversation_not_found"));
            };
            if workdir.is_empty() {
                return Err(Skip::new(RunState::Failed, "conversation_not_found"));
            }
            (machine_id, Some(conversation_id), Some(workdir))
        }
    };
    let registry = scheduler
        .registry()
        .ok_or_else(|| Skip::new(RunState::SkippedMachineOffline, "no_registry"))?;
    if registry.command_sender(machine_id).is_none() {
        return Err(Skip::new(RunState::SkippedMachineOffline, "worker_offline"));
    }

    let mut conn = scheduler
        .pool()
        .acquire()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    let executor = if let Some(conversation_id) = existing_conversation {
        let row: (String, Option<String>) = sqlx::query_as(
            "select c.harness, d.effective_model from conversations c
             left join conversation_derived d on d.conversation_id = c.id
             where c.id = $1",
        )
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "conversation_not_found"))?;
        ResolvedExecutor {
            harness: row.0,
            // Окно Fable существующей сессии — по её действующей модели.
            model: row.1.unwrap_or_default(),
            effort: String::new(),
            preset: "clean".to_owned(),
            mode: None,
        }
    } else {
        resolve_route(scheduler, &mut conn, schedule, run, run_anyway).await?
    };

    let provider = canterlot_proto::harness::provider_of(&executor.harness).unwrap_or("");
    // Зонд лимитов (`known`/`unknown`) едет в журнал запуска дальше.
    let mut limits_probe = json!({"limits": "unknown"});
    if !run_anyway && !provider.is_empty() {
        let (skip, probe) = limits_skip(
            &mut conn,
            schedule.owner_id,
            provider,
            &executor.model,
            existing_conversation,
            scheduler.limits_config(),
            Utc::now(),
        )
        .await?;
        limits_probe = probe;
        if let Some(detail) = skip {
            return Err(Skip::with(RunState::SkippedLimits, detail));
        }
    }
    // Соединение не держим через ожидание воркера: иначе каждый приём
    // с предпроверкой паркует соединение пула на минуту.
    drop(conn);

    if !run_anyway && let Some(command) = schedule.precheck.as_deref() {
        precheck(
            scheduler,
            registry,
            run,
            schedule,
            command,
            machine_id,
            existing_workdir.as_deref(),
        )
        .await?;
    }

    dispatch(
        scheduler,
        schedule,
        run,
        &executor,
        provider,
        run_anyway,
        manual,
        limits_probe,
    )
    .await
}

#[derive(Debug)]
struct ResolvedExecutor {
    harness: String,
    model: String,
    effort: String,
    preset: String,
    mode: Option<String>,
}

async fn resolve_route(
    scheduler: &Scheduler,
    conn: &mut PgConnection,
    schedule: &Schedule,
    run: &ScheduleRun,
    run_anyway: bool,
) -> Result<ResolvedExecutor, Skip> {
    let (task_type, level, explicit) = match &schedule.target {
        ScheduleTarget::NewSession {
            route,
            executor,
            preset,
            permission_mode,
            ..
        } => {
            let explicit_executor = executor.clone().map(|executor| ResolvedExecutor {
                harness: executor.harness,
                model: executor.model,
                effort: executor.effort,
                preset: preset.clone().unwrap_or_else(|| "clean".to_owned()),
                mode: permission_mode.clone(),
            });
            if let Some(resolved) = explicit_executor {
                return Ok(resolved);
            }
            let route = route
                .as_ref()
                .ok_or_else(|| Skip::new(RunState::Failed, "invalid_executor"))?;
            (
                route.task_type.clone(),
                route.level.clone(),
                permission_mode.clone(),
            )
        }
        ScheduleTarget::ExistingSession { .. } => unreachable!("existing sessions are not routed"),
    };
    let (policy, revision) = routing::load_policy(conn, schedule.owner_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "routing_policy_unavailable"))?;
    let (machine_id, workdir) = match &schedule.target {
        ScheduleTarget::NewSession {
            machine_id,
            workdir,
            ..
        } => (*machine_id, workdir.clone()),
        _ => unreachable!(),
    };
    let catalogs = routing::load_catalogs(conn, machine_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "machine_catalog_unavailable"))?;
    let whitelist = crate::session_tools::whitelist::view(conn, schedule.owner_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "whitelist_unavailable"))?
        .entries;
    let limits = routing::limits_view(conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
    let live = routing::live_counts(conn, schedule.owner_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "live_counts_unavailable"))?;
    let now = Utc::now();
    let request = RouteRequest {
        task_type,
        level,
        parent_harness: String::new(),
        parent_mode: String::new(),
        explicit_mode: explicit.clone(),
        workdir,
    };
    let decision = routing::route(RouteInputs {
        policy: &policy,
        revision,
        whitelist: &whitelist,
        catalogs: &catalogs,
        limits: &limits,
        live: &live,
        stale: scheduler.routing_stale(),
        now,
        request: &request,
        bypass_limits: run_anyway,
    })
    .map_err(|error| match error {
        RouteError::NoExecutor { considered, reset } => Skip::with(
            RunState::SkippedLimits,
            json!({"reason": "no_executor", "considered": considered, "reset": reset}),
        ),
        RouteError::UnknownTask { available } => Skip::with(
            RunState::Failed,
            json!({"reason": "unknown_task_type", "available": available}),
        ),
    })?;
    let preset =
        routing::resolve_policy_preset(conn, schedule.owner_id, decision.preset.as_deref())
            .await
            .map_err(|_| Skip::new(RunState::Failed, "preset_unavailable"))?;
    let _ = run;
    // Режим, проверенный маршрутизацией, едет и в создание: раньше проверка
    // видела дочерний запасной (`read_only` при пустом родителе), а создание
    // с `None` падало в `bypass` по умолчанию.
    let mode = explicit.clone().unwrap_or_else(|| {
        crate::session_tools::spawn::resolve_default_mode("", "", &decision.harness)
    });
    // Пресет расписания важнее политики: владелец выбрал его в редакторе.
    let preset = match &schedule.target {
        ScheduleTarget::NewSession {
            preset: schedule_preset,
            ..
        } => schedule_preset.clone().unwrap_or(preset),
        ScheduleTarget::ExistingSession { .. } => preset,
    };
    Ok(ResolvedExecutor {
        harness: decision.harness,
        model: decision.model,
        effort: decision.effort,
        preset,
        mode: Some(mode),
    })
}

async fn limits_skip(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    model: &str,
    existing_conversation: Option<Uuid>,
    config: &LimitsConfig,
    now: DateTime<Utc>,
) -> Result<(Option<Value>, Value), Skip> {
    let settings = limit_settings::effective(conn, owner_id, config)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limit_settings_unavailable"))?;
    let all = limits::current(conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
    // Строгое сравнение остатка в промилле, как PWA (`remainingPct`):
    // целое деление роняло 951‰ при пороге 5% в «не красное».
    let mut probe = json!({"limits": "unknown"});
    if let Some(entry) = all.iter().find(|entry| entry.provider == provider) {
        let mut known = false;
        // Окно Fable — только для Claude-моделей Fable, как в маршрутизации.
        let fable =
            provider == "claude" && model.to_ascii_lowercase().contains("fable");
        let mut windows: Vec<(&str, Option<&limits::WindowState>)> = vec![
            ("five_hour", entry.five_hour.as_ref()),
            ("weekly", entry.weekly.as_ref()),
        ];
        if fable {
            windows.push(("weekly_fable", entry.weekly_fable.as_ref()));
        }
        for (name, window) in windows {
            let Some(window) = window else { continue };
            // Окно с прошедшим сбросом несвежее, даже если показание недавнее.
            if window.resets_at.is_some_and(|reset| reset <= now) {
                continue;
            }
            let fresh = TimeDelta::from_std(config.stale_after).unwrap_or(TimeDelta::hours(1));
            if now - window.observed_at > fresh {
                continue;
            }
            known = true;
            let remaining = 100.0 - f64::from(window.used_permille) / 10.0;
            if remaining < f64::from(settings.danger_remaining_pct) {
                return Ok((
                    Some(json!({"reason": "red_window", "window": name, "remaining_pct": remaining})),
                    probe,
                ));
            }
        }
        if known {
            probe = json!({"limits": "known"});
        }
    }
    if let Some(conversation_id) = existing_conversation {
        let (holds, override_active): (sqlx::types::Json<Vec<HoldReason>>, bool) = sqlx::query_as(
            "select hold_reasons,coalesce(reserve_override_until>clock_timestamp(),false)
             from queue_state join conversations on conversations.id=queue_state.conversation_id
             where queue_state.conversation_id=$1",
        )
        .bind(conversation_id)
        .fetch_one(conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "queue_unavailable"))?;
        if !override_active
            && holds.0.iter().any(|hold| {
                matches!(
                    hold,
                    HoldReason::QuotaExhausted { provider: p, .. } | HoldReason::WeeklyReserve { provider: p, .. }
                        if p == provider
                )
            })
        {
            return Ok((Some(json!({"reason": "queue_hold", "provider": provider})), probe));
        }
    } else {
        limits::share_provider(conn, provider)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
        let holds = crunch_holds(conn, provider).await?;
        if !holds.is_empty() {
            return Ok((
                Some(json!({"reason": "initial_hold", "provider": provider})),
                probe,
            ));
        }
    }
    Ok((None, probe))
}

async fn crunch_holds(conn: &mut PgConnection, provider: &str) -> Result<Vec<HoldReason>, Skip> {
    crate::crunch::initial_holds_in_tx(conn, provider)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))
}

async fn precheck(
    scheduler: &Scheduler,
    registry: &crate::session::Registry,
    run: &ScheduleRun,
    schedule: &Schedule,
    command: &str,
    machine_id: Uuid,
    existing_workdir: Option<&str>,
) -> Result<(), Skip> {
    let attempt_id = Uuid::now_v7();
    let mut conn = scheduler
        .pool()
        .acquire()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    set_run_state(
        &mut conn,
        run.id,
        RunState::Prechecking,
        json!({"precheck_attempt_id": attempt_id, "precheck_started_at": Utc::now()}),
        None,
        None,
        None,
        None,
    )
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    drop(conn);
    let (workdir, timeout_seconds) = match &schedule.target {
        ScheduleTarget::NewSession { workdir, .. } => {
            (workdir.clone(), schedule.precheck_timeout_seconds)
        }
        ScheduleTarget::ExistingSession { .. } => {
            let Some(workdir) = existing_workdir else {
                return Err(Skip::new(RunState::Failed, "conversation_not_found"));
            };
            (workdir.to_owned(), schedule.precheck_timeout_seconds)
        }
    };
    let request = canterlot_proto::worker::v1::SchedulePrecheck {
        attempt_id: attempt_id.to_string(),
        workdir,
        command: command.to_owned(),
        timeout_seconds: timeout_seconds as u32,
    };
    let wait =
        std::time::Duration::from_secs(timeout_seconds as u64) + std::time::Duration::from_secs(5);
    let reply = match registry.ask_precheck(machine_id, request, wait).await {
        Ok(reply) => reply,
        Err(PrecheckFailure::CapabilityMissing) => {
            return Err(Skip::with(
                RunState::Failed,
                json!({"reason": "precheck_capability_missing", "capability": CAPABILITY_SCHEDULE_PRECHECK_V1}),
            ));
        }
        Err(PrecheckFailure::NoSession | PrecheckFailure::NoAnswer | PrecheckFailure::Busy) => {
            return Err(Skip::new(RunState::Failed, "precheck_result_unknown"));
        }
        Err(PrecheckFailure::Invalid) => {
            return Err(Skip::new(RunState::Failed, "precheck_invalid_attempt"));
        }
    };
    if reply.attempt_id != attempt_id.to_string() {
        return Err(Skip::new(RunState::Failed, "precheck_attempt_mismatch"));
    }
    match reply.outcome {
        Some(canterlot_proto::worker::v1::precheck_result::Outcome::Finished(finished)) => {
            if finished.exit_code == 0 && !finished.timed_out {
                Ok(())
            } else {
                Err(Skip::with(
                    RunState::SkippedPrecheck,
                    json!({
                        "reason": if finished.timed_out { "precheck_timeout" } else { "precheck_nonzero" },
                        "exit_code": finished.exit_code,
                        "output_tail": finished.output_tail,
                    }),
                ))
            }
        }
        Some(canterlot_proto::worker::v1::precheck_result::Outcome::Error(error)) => {
            Err(Skip::with(
                RunState::Failed,
                json!({"reason": "precheck_error", "detail": error.reason}),
            ))
        }
        None => Err(Skip::new(RunState::Failed, "precheck_empty_reply")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    scheduler: &Scheduler,
    schedule: &Schedule,
    run: &ScheduleRun,
    executor: &ResolvedExecutor,
    provider: &str,
    run_anyway: bool,
    manual: bool,
    limits_probe: Value,
) -> AdmitResult {
    let mut tx = crate::db::begin(scheduler.pool())
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    let current: Option<super::ScheduleRow> =
        sqlx::query_as("select * from schedules where id=$1 and owner_id=$2 for update")
            .bind(schedule.id)
            .bind(schedule.owner_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    let Some(current) = current else {
        let _ = tx.rollback().await;
        return Err(Skip::new(RunState::SkippedChanged, "schedule_deleted"));
    };
    // Повтор после сбоя сразу после commit dispatch: не создаём вторую сессию
    // и не ставим второй ввод; возвращаем уже зафиксированные идентификаторы.
    // Строка запуска читается под замком расписания, после него: две
    // параллельные приёмки не разойдутся мимо друг друга.
    let already: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select session_id,input_id from schedule_runs where id=$1 and state='started' for update",
    )
    .bind(run.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    if let Some((session_id, input_id)) = already {
        tx.commit()
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
        return Ok((session_id, input_id, json!({"replayed": true})));
    }
    let current = current
        .into_schedule()
        .map_err(|_| Skip::new(RunState::Failed, "invalid_schedule"))?;
    // Ручной запуск с кнопки разрешён и на приостановленном расписании:
    // важно лишь, что его не правили посреди приёма (ревизия та же).
    // Пауза во время приёма тихого тика — это `skipped_changed`.
    if current.revision != run.claimed_revision {
        let _ = tx.rollback().await;
        return Err(Skip::new(
            RunState::SkippedChanged,
            "schedule_revision_changed",
        ));
    }
    if !manual && !run_anyway && !current.enabled {
        let _ = tx.rollback().await;
        return Err(Skip::new(RunState::SkippedChanged, "schedule_paused"));
    }
    // Замок владельца как у прочих запускаемых сессий: параллельные приёмки
    // одного владельца упорядочиваются здесь, а не расходятся в две сессии.
    crate::conversations::lock_owner(&mut tx, schedule.owner_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    // Решение о пересечении повторяется под замками dispatch: ранняя проверка
    // в приёме могла пропустить заявку, чей ввод встал в очередь после неё.
    if !run_anyway && !manual && current.overlap == OverlapPolicy::Skip
        && earlier_blocking(&mut tx, schedule.id, run.id).await? > 0
    {
        let _ = tx.rollback().await;
        return Err(Skip::new(RunState::SkippedOverlap, "earlier_run_in_flight"));
    }
    // Пересчёт параллельности с корнями: `live_counts` в маршрутизации видит
    // только детей (`parent_conversation_id is not null`), а запланированные
    // сессии — корни, как и интерактивные. Два расписания под `max_parallel=1`
    // иначе проходят проверку одновременно.
    if matches!(&schedule.target, ScheduleTarget::NewSession { .. }) {
        let (policy, _) = routing::load_policy(&mut tx, schedule.owner_id)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "routing_policy_unavailable"))?;
        let pair = format!("{}/{}", executor.harness, executor.model);
        if let Some(limit) = policy.max_parallel.get(&pair) {
            let busy: i64 = sqlx::query_scalar(
                "select count(*) from conversations c
                  join conversation_derived d on d.conversation_id = c.id
                  where c.owner_id = $1 and c.harness = $2 and c.model = $3
                    and c.trashed_at is null
                    and (d.activity_state in ('working','stalled')
                         or (d.activity_state = 'idle' and d.queue_queued > 0)
                         or (exists (select 1 from executions e
                                     where e.conversation_id = c.id
                                       and e.state not in ('stopped','failed','owner_lost'))
                             and (exists (select 1 from inputs i
                                          where i.conversation_id = c.id and i.feed_seq is null
                                            and i.state in ('queued','dispatched','on_machine'))
                                  or exists (select 1 from queue_state q
                                             where q.conversation_id = c.id and q.run_slot <> 'free'))))",
            )
            .bind(schedule.owner_id)
            .bind(&executor.harness)
            .bind(&executor.model)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
            if busy >= *limit {
                let _ = tx.rollback().await;
                return Err(Skip::with(
                    RunState::SkippedLimits,
                    json!({"reason": "parallel_busy", "pair": pair, "busy": busy, "limit": limit}),
                ));
            }
        }
    }
    // Повторная проверка лимитов внутри транзакции dispatch под замком
    // расписания: между ранней проверкой и commit лежит ожидание воркера,
    // окна и удержания могли покраснеть. Та же проверка, что и до предпроверки.
    let mut limits_probe = limits_probe;
    if !run_anyway && !provider.is_empty() {
        let existing = match &schedule.target {
            ScheduleTarget::ExistingSession { conversation_id } => Some(*conversation_id),
            ScheduleTarget::NewSession { .. } => None,
        };
        let (skip, probe) = limits_skip(
            &mut tx,
            schedule.owner_id,
            provider,
            &executor.model,
            existing,
            scheduler.limits_config(),
            Utc::now(),
        )
        .await?;
        limits_probe = probe;
        if let Some(detail) = skip {
            let _ = tx.rollback().await;
            return Err(Skip::with(RunState::SkippedLimits, detail));
        }
    }
    let now = Utc::now();
    let content = vec![ContentBlock::Text {
        text: schedule.prompt.clone(),
    }];
    let (session_id, input_id) = match &schedule.target {
        ScheduleTarget::NewSession {
            machine_id,
            workdir,
            group_id,
            ..
        } => {
            let title = render_title(&schedule.title_template, &schedule.name, run.scheduled_at);
            let created = conversations::create_in_tx(
                &mut tx,
                NewConversation {
                    owner_id: schedule.owner_id,
                    machine_id: *machine_id,
                    harness: executor.harness.clone(),
                    workdir: workdir.clone(),
                    model: executor.model.clone(),
                    effort: executor.effort.clone(),
                    permission_mode: executor.mode.clone(),
                    instruction_profile: Some(executor.preset.clone()),
                    title: Some(title),
                    color: None,
                    group_ids: group_id.iter().copied().collect(),
                    crunch: CrunchSetting::default(),
                    start: true,
                    resume_native_id: None,
                    parent: None,
                    worktree_source: None,
                },
                LimitsConfig::default(),
                Uuid::now_v7(),
                scheduler.registry().expect("registry checked earlier"),
            )
            .await
            .map_err(|error| {
                Skip::with(
                    RunState::Failed,
                    json!({"reason": "create_failed", "detail": format!("{error:?}")}),
                )
            })?
            .conversation_id;
            let submitted = submit_message_in_tx(
                &mut tx,
                &SubmitMessage {
                    owner_id: schedule.owner_id,
                    conversation_id: created,
                    key: format!("scheduled-input:{}", run.id),
                    content,
                    attachment_ids: Vec::new(),
                    retry_of: None,
                    received_at: run.scheduled_at,
                },
                Uuid::now_v7(),
            )
            .await
            .map_err(|error| {
                Skip::with(
                    RunState::Failed,
                    json!({"reason": "submit_failed", "detail": format!("{error:?}")}),
                )
            })?;
            (created, submitted.input_id)
        }
        ScheduleTarget::ExistingSession { conversation_id } => {
            let submitted = submit_message_in_tx(
                &mut tx,
                &SubmitMessage {
                    owner_id: schedule.owner_id,
                    conversation_id: *conversation_id,
                    key: format!("scheduled-input:{}", run.id),
                    content,
                    attachment_ids: Vec::new(),
                    retry_of: None,
                    received_at: run.scheduled_at,
                },
                Uuid::now_v7(),
            )
            .await
            .map_err(|error| {
                Skip::with(
                    RunState::Failed,
                    json!({"reason": "submit_failed", "detail": format!("{error:?}")}),
                )
            })?;
            (*conversation_id, submitted.input_id)
        }
    };
    let delay = delay_ms(run.scheduled_at, now);
    // «Запустить в любом случае» обходит приёмку, но crunch всё равно может
    // держать поставленный ввод: фиксируем удержание в журнале запуска.
    let mut final_detail = if run_anyway {
        json!({"reason": "run_anyway", "provider": provider})
    } else {
        let mut base = json!({"provider": provider});
        merge(&mut base, limits_probe);
        base
    };
    if run_anyway
        && let Ok(holds) = sqlx::query_scalar::<_, Value>(
            "select to_jsonb(hold_reasons) from queue_state where conversation_id=$1",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await
        && let Some(holds) = holds
        && holds != json!([])
    {
        final_detail["queue_hold"] = holds;
    }
    set_run_state(
        &mut tx,
        run.id,
        RunState::Started,
        final_detail.clone(),
        Some(now),
        Some(delay),
        Some(session_id),
        Some(input_id),
    )
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    tx.commit()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    scheduler.dispatcher().trigger(session_id);
    Ok((session_id, input_id, final_detail))
}

/// Тот же advisory-замок, что `idempotency::run_in_tx`: ключ первым,
/// затем доменные строки. Короткая резервная транзакция его не держит
/// дольше вставки.
async fn lock_idempotency_key(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    owner_id: Uuid,
    key: &str,
) -> Result<(), sqlx::Error> {
    use sha2::{Digest, Sha256};
    let mut lock_hash = Sha256::new();
    lock_hash.update(b"canterlot:idempotency:");
    lock_hash.update(owner_id.as_bytes());
    lock_hash.update(key.as_bytes());
    let lock_id = i64::from_be_bytes(lock_hash.finalize()[..8].try_into().expect("eight bytes"));
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(lock_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Проигравший гонку резерва дожидается исхода победителя и возвращает
/// тот же запуск: повтор не создаёт второй dispatch. Соединение пула
/// между опросами не держится.
async fn await_winner_run(
    pool: &PgPool,
    owner_id: Uuid,
    run_id: Uuid,
) -> Result<ScheduleRun, ScheduleError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    let mut conn = pool.acquire().await?;
    let Some(mut current) = super::get_run(&mut conn, owner_id, run_id).await? else {
        return Err(ScheduleError::NotFound);
    };
    drop(conn);
    loop {
        if !matches!(
            current.state,
            RunState::Claimed | RunState::Prechecking
        ) {
            return Ok(current);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(current);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut conn = pool.acquire().await?;
        let refreshed = super::get_run(&mut conn, owner_id, run_id).await?;
        if let Some(refreshed) = refreshed {
            current = refreshed;
        } else {
            return Err(ScheduleError::NotFound);
        }
    }
}
/// «Запустить сейчас»: тот же приём и dispatch, но под ключом клиента.
/// Резерв строки запуска атомарен (уникальность `0112_schedule_run_key`):
/// параллельные запросы с тем же ключом и повтор после обрыва получают
/// тот же запуск вместо второго dispatch.
pub async fn run_now(
    state: &AppState,
    owner_id: Uuid,
    schedule_id: Uuid,
    key: &str,
    run_anyway: bool,
    correlation_id: Uuid,
) -> Result<ScheduleRun, ScheduleError> {
    let body = json!({"schedule_id": schedule_id, "run_anyway": run_anyway});
    let hash = crate::idempotency::request_hash("run_now", &body);
    // Короткая резервная транзакция: тот же advisory-замок, что `run_in_tx`,
    // затем поиск/создание резерва. Долгой работы с воркером внутри нет.
    let reserved = {
        let mut tx = crate::db::begin(&state.pool).await?;
        lock_idempotency_key(&mut tx, owner_id, key).await?;
        match crate::idempotency::lookup(&mut tx, owner_id, key, "run_now", &hash).await? {
            crate::idempotency::Begin::Replay(value) => {
                let _ = tx.rollback().await;
                return serde_json::from_value(value)
                    .map_err(|e| ScheduleError::Db(sqlx::Error::Decode(Box::new(e))));
            }
            crate::idempotency::Begin::Conflict => {
                let _ = tx.rollback().await;
                return Err(ScheduleError::Conflict("idempotency_conflict"));
            }
            crate::idempotency::Begin::New => {}
        }
        let schedule = super::get(&mut tx, owner_id, schedule_id)
            .await?
            .ok_or(ScheduleError::NotFound)?;
        let now = Utc::now();
        let reserved = super::reserve_run(
            &mut tx,
            Uuid::now_v7(),
            schedule_id,
            owner_id,
            now,
            schedule.revision,
            key,
            json!({"manual": true, "run_anyway": run_anyway}),
        )
        .await?;
        match reserved {
            Some(run) => {
                tx.commit().await?;
                Some((schedule, run))
            }
            None => {
                // Чужой резерв с тем же ключом: дожидаемся победителя.
                let winner = super::get_run_by_key(&mut tx, owner_id, schedule_id, key).await?;
                tx.rollback().await?;
                let Some(winner) = winner else {
                    return Err(ScheduleError::NotFound);
                };
                let winner_id = winner.id;
                return await_winner_run(&state.pool, owner_id, winner_id).await;
            }
        }
    };
    let Some((schedule, run)) = reserved else {
        return Err(ScheduleError::NotFound);
    };
    let scheduler = state.scheduler.clone();
    let claimed = ClaimedRun {
        run: run.clone(),
        schedule,
    };
    let result = admit_inner(&scheduler, &claimed.schedule, &run, run_anyway, true).await;
    let (state_out, detail, session_id, input_id, started_at, delay) = match result {
        Ok((session_id, input_id, dispatch_detail)) => {
            let started_at = Utc::now();
            let mut detail = run.detail.clone();
            merge(&mut detail, dispatch_detail);
            (
                RunState::Started,
                detail,
                Some(session_id),
                Some(input_id),
                Some(started_at),
                Some(delay_ms(run.scheduled_at, started_at)),
            )
        }
        Err(skip) => {
            let mut detail = run.detail.clone();
            merge(&mut detail, skip.detail);
            (skip.state, detail, None, None, None, None)
        }
    };
    let mut tx = crate::db::begin(&state.pool).await?;
    set_run_state(
        &mut tx,
        run.id,
        state_out,
        detail.clone(),
        started_at,
        delay,
        session_id,
        input_id,
    )
    .await?;
    let updated = apply_outcome(
        &run,
        state_out,
        detail,
        session_id,
        input_id,
        started_at,
        delay,
    );
    let response = serde_json::to_value(&updated)
        .map_err(|e| ScheduleError::Db(sqlx::Error::Encode(Box::new(e))))?;
    events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Schedules { owner_id },
            kind: "schedule_run_changed",
            conversation_id: None,
            execution_id: None,
            payload: response.clone(),
            correlation_id,
        },
    )
    .await?;
    crate::idempotency::record(&mut tx, owner_id, key, "run_now", &hash, &response, None).await?;
    if state_out == RunState::Failed {
        let reason = updated
            .detail
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        crate::notify::enqueue_schedule_failure_in_tx(
            &mut tx,
            schedule_id,
            &claimed.schedule.name,
            reason,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(updated)
}

#[allow(dead_code)]
fn _pool_marker(_: &PgPool) {}
#[allow(dead_code)]
fn _delta_marker(_: TimeDelta) {}
