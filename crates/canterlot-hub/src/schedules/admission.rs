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
    delay_ms, in_flight, insert_run, render_title, set_run_state,
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
    let mut conn = match scheduler.pool().acquire().await {
        Ok(conn) => conn,
        Err(error) => {
            tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
            return;
        }
    };
    if let Err(error) = set_run_state(
        &mut conn, run.id, state, detail, started_at, delay, session_id, input_id,
    )
    .await
    {
        tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
        return;
    }
    let payload = json!({"run_id": run.id, "schedule_id": schedule.id, "state": state.as_str()});
    if let Err(error) = events::append(
        &mut conn,
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
        tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot publish run outcome");
    }
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(object), Some(extra)) = (target.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
}

async fn admit_inner(
    scheduler: &Scheduler,
    schedule: &Schedule,
    run: &ScheduleRun,
    run_anyway: bool,
    manual: bool,
) -> AdmitResult {
    if !run_anyway && !manual && schedule.overlap == OverlapPolicy::Skip {
        let mut conn = scheduler
            .pool()
            .acquire()
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
        if in_flight(&mut conn, schedule.id).await.unwrap_or(0) > 1 {
            return Err(Skip::new(RunState::SkippedOverlap, "earlier_run_in_flight"));
        }
        let delivered: i64 = sqlx::query_scalar(
            "select count(*) from schedule_runs r join inputs i on i.id=r.input_id
             where r.schedule_id=$1 and r.id<>$2 and i.state='queued'",
        )
        .bind(schedule.id)
        .bind(run.id)
        .fetch_one(&mut *conn)
        .await
        .unwrap_or(0);
        if delivered > 0 {
            return Err(Skip::new(
                RunState::SkippedOverlap,
                "earlier_input_not_delivered",
            ));
        }
    }

    let (machine_id, existing_conversation) = match &schedule.target {
        ScheduleTarget::NewSession { machine_id, .. } => (*machine_id, None),
        ScheduleTarget::ExistingSession { conversation_id } => {
            let mut conn = scheduler
                .pool()
                .acquire()
                .await
                .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
            let row: Option<(Uuid, Uuid)> = sqlx::query_as(
                "select c.machine_id,q.conversation_id from conversations c
                 join queue_state q on q.conversation_id=c.id
                 where c.id=$1 and c.owner_id=$2 and c.trashed_at is null",
            )
            .bind(conversation_id)
            .bind(schedule.owner_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
            let Some((machine_id, conversation_id)) = row else {
                return Err(Skip::new(RunState::Failed, "conversation_not_found"));
            };
            (machine_id, Some(conversation_id))
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
        let harness: String = sqlx::query_scalar("select harness from conversations where id=$1")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "conversation_not_found"))?;
        ResolvedExecutor {
            harness,
            model: String::new(),
            effort: String::new(),
            preset: "clean".to_owned(),
            mode: None,
        }
    } else {
        resolve_route(scheduler, &mut conn, schedule, run, run_anyway).await?
    };

    let provider = canterlot_proto::harness::provider_of(&executor.harness).unwrap_or("");
    if !run_anyway
        && !provider.is_empty()
        && let Some(detail) = limits_skip(
            &mut conn,
            schedule.owner_id,
            provider,
            &executor.model,
            existing_conversation,
            scheduler.limits_config(),
            Utc::now(),
        )
        .await?
    {
        return Err(Skip::with(RunState::SkippedLimits, detail));
    }

    if !run_anyway && let Some(command) = schedule.precheck.as_deref() {
        precheck(scheduler, registry, run, schedule, command, machine_id).await?;
    }

    dispatch(scheduler, schedule, run, &executor, provider, run_anyway).await
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
    Ok(ResolvedExecutor {
        harness: decision.harness,
        model: decision.model,
        effort: decision.effort,
        preset,
        mode: explicit,
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
) -> Result<Option<Value>, Skip> {
    let settings = limit_settings::effective(conn, owner_id, config)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limit_settings_unavailable"))?;
    let all = limits::current(conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
    let mut detail = json!({"limits": "unknown"});
    if let Some(entry) = all.iter().find(|entry| entry.provider == provider) {
        let mut known = false;
        let fable = model.to_ascii_lowercase().contains("fable");
        let mut windows: Vec<(&str, Option<&limits::WindowState>)> = vec![
            ("five_hour", entry.five_hour.as_ref()),
            ("weekly", entry.weekly.as_ref()),
        ];
        if fable {
            windows.push(("weekly_fable", entry.weekly_fable.as_ref()));
        }
        for (name, window) in windows {
            let Some(window) = window else { continue };
            let fresh = TimeDelta::from_std(config.stale_after).unwrap_or(TimeDelta::hours(1));
            if now - window.observed_at > fresh {
                continue;
            }
            known = true;
            let remaining = 100_i32 - i32::from(window.used_permille) / 10;
            if remaining < i32::from(settings.danger_remaining_pct) {
                return Ok(Some(
                    json!({"reason": "red_window", "window": name, "remaining_pct": remaining}),
                ));
            }
        }
        if known {
            detail = json!({"limits": "known"});
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
            return Ok(Some(json!({"reason": "queue_hold", "provider": provider})));
        }
    } else {
        limits::share_provider(conn, provider)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
        let holds = crunch_holds(conn, provider).await?;
        if !holds.is_empty() {
            return Ok(Some(
                json!({"reason": "initial_hold", "provider": provider}),
            ));
        }
    }
    let _ = detail;
    Ok(None)
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
            (String::new(), schedule.precheck_timeout_seconds)
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

async fn dispatch(
    scheduler: &Scheduler,
    schedule: &Schedule,
    run: &ScheduleRun,
    executor: &ResolvedExecutor,
    provider: &str,
    run_anyway: bool,
) -> AdmitResult {
    let mut tx = crate::db::begin(scheduler.pool())
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    // Повтор после сбоя сразу после commit dispatch: не создаём вторую сессию
    // и не ставим второй ввод; возвращаем уже зафиксированные идентификаторы.
    let already: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select session_id,input_id from schedule_runs where id=$1 and state='started'",
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
    let current = current
        .into_schedule()
        .map_err(|_| Skip::new(RunState::Failed, "invalid_schedule"))?;
    if !current.enabled || current.revision != run.claimed_revision {
        let _ = tx.rollback().await;
        return Err(Skip::new(
            RunState::SkippedChanged,
            "schedule_revision_changed",
        ));
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
    let final_detail = if run_anyway {
        json!({"reason": "run_anyway", "provider": provider})
    } else {
        json!({"provider": provider})
    };
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

/// «Запустить сейчас»: тот же приём и dispatch, но под ключом клиента.
pub async fn run_now(
    state: &AppState,
    owner_id: Uuid,
    schedule_id: Uuid,
    key: &str,
    run_anyway: bool,
    correlation_id: Uuid,
) -> Result<ScheduleRun, ScheduleError> {
    let mut conn = state.pool.acquire().await?;
    let body = json!({"schedule_id": schedule_id, "run_anyway": run_anyway});
    let hash = crate::idempotency::request_hash("run_now", &body);
    match crate::idempotency::lookup(&mut conn, owner_id, key, "run_now", &hash).await? {
        crate::idempotency::Begin::Replay(value) => {
            return serde_json::from_value(value)
                .map_err(|e| ScheduleError::Db(sqlx::Error::Decode(Box::new(e))));
        }
        crate::idempotency::Begin::Conflict => {
            return Err(ScheduleError::Conflict("idempotency_conflict"));
        }
        crate::idempotency::Begin::New => {}
    }
    let schedule = super::get(&mut conn, owner_id, schedule_id)
        .await?
        .ok_or(ScheduleError::NotFound)?;
    let now = Utc::now();
    let run = insert_run(
        &mut conn,
        Uuid::now_v7(),
        schedule_id,
        owner_id,
        now,
        schedule.revision,
        RunState::Claimed,
        true,
        key,
        json!({"manual": true, "run_anyway": run_anyway}),
    )
    .await?;
    drop(conn);
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
    let mut conn = state.pool.acquire().await?;
    set_run_state(
        &mut conn,
        run.id,
        state_out,
        detail.clone(),
        started_at,
        delay,
        session_id,
        input_id,
    )
    .await?;
    events::append(
        &mut conn,
        NewEvent {
            scope: Scope::Schedules { owner_id },
            kind: "schedule_run_changed",
            conversation_id: None,
            execution_id: None,
            payload: json!({"run_id": run.id, "schedule_id": schedule_id, "state": state_out.as_str()}),
            correlation_id,
        },
    )
    .await?;
    let mut updated = run;
    updated.state = state_out;
    updated.session_id = session_id;
    updated.input_id = input_id;
    updated.started_at = started_at;
    updated.delay_ms = delay;
    updated.detail = detail;
    let response = serde_json::to_value(&updated)
        .map_err(|e| ScheduleError::Db(sqlx::Error::Encode(Box::new(e))))?;
    crate::idempotency::record(&mut conn, owner_id, key, "run_now", &hash, &response, None).await?;
    Ok(updated)
}

#[allow(dead_code)]
fn _pool_marker(_: &PgPool) {}
#[allow(dead_code)]
fn _delta_marker(_: TimeDelta) {}
