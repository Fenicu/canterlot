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
    delay_ms, render_title,
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

/// Успех приёма: dispatch уже закоммитил `started` вместе с событием (14),
/// финалу писать нечего. Неуспех несёт состояние, из которого финал пишет.
struct AdmitOk;
struct AdmitStop {
    skip: Skip,
    /// Состояние, из которого финал условно пишет (A): `claimed` без
    /// предпроверки, `prechecking` после неё.
    from: RunState,
}
/// `Superseded` — строкой уже владеет другой (восстановление или вторая
/// приёмка): ничего не пишем, исход чужой.
enum AdmitEnd {
    Stop(AdmitStop),
    Superseded,
}
type AdmitResult = Result<AdmitOk, AdmitEnd>;

fn stop(skip: Skip, from: RunState) -> AdmitEnd {
    AdmitEnd::Stop(AdmitStop { skip, from })
}

/// Обработка одного `claimed`-запуска. Все проверки идут вне транзакции тика.
/// Владение строкой — попытка из заявки (A): каждый переход условный,
/// проигрыш — молчаливый выход, исход пишет победитель.
pub async fn admit(scheduler: &Scheduler, claimed: ClaimedRun) {
    let _guard = super::AdmittingGuard::insert(claimed.run.id);
    let run = claimed.run;
    let schedule = claimed.schedule;
    let attempt = run.attempt_id;
    let result = admit_inner(scheduler, &schedule, &run, run.run_anyway, run.manual).await;
    let finish = match result {
        // Dispatch закоммитил `started` и событие сам: финалу делать нечего.
        Ok(_) => return,
        Err(AdmitEnd::Superseded) => return,
        Err(AdmitEnd::Stop(stop)) => stop,
    };
    let mut detail = run.detail.clone();
    merge(&mut detail, finish.skip.detail);
    // Финал под замком расписания (порядок B: расписание, затем запуск),
    // переход условный (A): чужую строку не перезаписываем.
    let mut tx = match crate::db::begin(scheduler.pool()).await {
        Ok(tx) => tx,
        Err(error) => {
            tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
            return;
        }
    };
    let schedule_exists: bool = match sqlx::query_scalar(
        "select exists(select 1 from schedules where id=$1 for update)",
    )
    .bind(schedule.id)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(exists) => exists,
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
            return;
        }
    };
    if !schedule_exists {
        // Расписание удалили, пока шёл приём: строка запуска ушла
        // каскадом, событие о призраке не публикуем.
        let _ = tx.rollback().await;
        return;
    }
    let moved = match super::transition_run(
        &mut tx,
        run.id,
        finish.from,
        attempt,
        finish.skip.state,
        detail.clone(),
        None,
        None,
        None,
        None,
    )
    .await
    {
        Ok(moved) => moved,
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
            return;
        }
    };
    if !moved {
        // Строку забрало восстановление или вторая приёмка: исход чужой.
        let _ = tx.rollback().await;
        return;
    }
    let updated = apply_outcome(
        &run,
        finish.skip.state,
        detail.clone(),
        None,
        None,
        None,
        None,
    );
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
    if finish.skip.state == RunState::Failed {
        let reason = detail
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("failed");
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
        run_anyway: run.run_anyway,
        attempt_id: run.attempt_id,
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
/// (`queued/dispatched/on_machine/transmitting/acceptance_unconfirmed`)
/// или чья сессия работает/ждёт владельца. Поздние заявки ранних не блокируют.
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
                    and (i.state in ('queued','dispatched','on_machine','transmitting','acceptance_unconfirmed')
                         or d.activity_state in ('working','waiting_for_user','stalled'))))",
    )
    .bind(schedule_id)
    .bind(run_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
}

impl From<Skip> for AdmitEnd {
    fn from(skip: Skip) -> Self {
        stop(skip, RunState::Claimed)
    }
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
    static ADMIT_SEMAPHORE: std::sync::OnceLock<tokio::sync::Semaphore> =
        std::sync::OnceLock::new();
    let semaphore = ADMIT_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(8));
    let _permit = semaphore
        .acquire()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    // Продлеваем аренду владения (A): восстановление меряет возраст от
    // последней смены владельца и не заберёт строку посреди приёма.
    // Ноль строк — строкой уже владеет другой: выходим молча.
    {
        let mut conn = scheduler
            .pool()
            .acquire()
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
        let owned = sqlx::query(
            "update schedule_runs set lease_at=clock_timestamp(),updated_at=clock_timestamp()
              where id=$1 and state='claimed'
                and ((attempt_id=$2) or (attempt_id is null and $2 is null))",
        )
        .bind(run.id)
        .bind(run.attempt_id)
        .execute(&mut *conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?
        .rows_affected();
        if owned == 0 {
            return Err(AdmitEnd::Superseded);
        }
    }
    if !run_anyway && !manual && schedule.overlap == OverlapPolicy::Skip {
        let mut conn = scheduler
            .pool()
            .acquire()
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
        if earlier_blocking(&mut conn, schedule.id, run.id).await? > 0 {
            return Err(Skip::new(RunState::SkippedOverlap, "earlier_run_in_flight").into());
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
                return Err(Skip::new(RunState::Failed, "conversation_not_found").into());
            };
            if workdir.is_empty() {
                return Err(Skip::new(RunState::Failed, "conversation_not_found").into());
            }
            (machine_id, Some(conversation_id), Some(workdir))
        }
    };
    let registry = scheduler
        .registry()
        .ok_or_else(|| Skip::new(RunState::SkippedMachineOffline, "no_registry"))?;
    if registry.command_sender(machine_id).is_none() {
        return Err(Skip::new(RunState::SkippedMachineOffline, "worker_offline").into());
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
            return Err(Skip::with(RunState::SkippedLimits, detail).into());
        }
    }
    // Соединение не держим через ожидание воркера: иначе каждый приём
    // с предпроверкой паркует соединение пула на минуту.
    drop(conn);

    let mut from = RunState::Claimed;
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
        from = RunState::Prechecking;
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
        from,
    )
    .await
}

#[derive(Debug, Clone)]
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
    // Занятость считаем с корнями (F): запланированные сессии — корни, как
    // интерактивные. Занятая пара отбрасывает кандидата в маршрутизации,
    // и выбор падает на свободный запасной, а не в отказ на dispatch.
    let live = routing::live_counts_with_roots(conn, schedule.owner_id)
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
        schedule_rules: true,
    })
    .map_err(|error| match error {
        // Только лимитные вердикты — пропуск по лимитам; остальные
        // (не в списке, не тот уровень, каталога нет) — ошибка расписания.
        RouteError::NoExecutor { considered, reset } => {
            let limit_only = !considered.is_empty()
                && considered.iter().all(|entry| {
                    matches!(
                        entry.verdict.as_str(),
                        "limits_over"
                            | "projected_over_threshold"
                            | "quota_paused"
                            | "limits_unknown"
                            | "parallel_busy"
                    )
                });
            if limit_only {
                Skip::with(
                    RunState::SkippedLimits,
                    json!({"reason": "no_executor", "considered": considered, "reset": reset}),
                )
            } else {
                Skip::with(
                    RunState::Failed,
                    json!({"reason": "no_executor", "considered": considered, "reset": reset}),
                )
            }
        }
        RouteError::UnknownTask { available } => Skip::with(
            RunState::Failed,
            json!({"reason": "unknown_task_type", "available": available}),
        ),
    })?;
    let _ = run;
    // Корневые умолчания нового сеанса (10), а не дочерние и не политика
    // делегирования: пропущенные режим и пресет — как на экране создания.
    let mode = explicit
        .clone()
        .unwrap_or_else(|| crate::conversations::DEFAULT_PERMISSION_MODE.to_owned());
    let preset = match &schedule.target {
        ScheduleTarget::NewSession {
            preset: schedule_preset,
            ..
        } => schedule_preset.clone().unwrap_or_else(|| "clean".to_owned()),
        ScheduleTarget::ExistingSession { .. } => "clean".to_owned(),
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
    // Замки раньше чтений (8): в транзакции dispatch они держатся до commit,
    // поэтому окно не краснеет и удержание не встаёт между чтением и замком.
    // Вне транзакции каждый оператор и так атомарен, порядок безвреден.
    if let Some(conversation_id) = existing_conversation {
        sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "queue_unavailable"))?;
    } else if !provider.is_empty() {
        limits::share_provider(conn, provider)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
    }
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
        let fable = provider == "claude" && model.to_ascii_lowercase().contains("fable");
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
                    Some(
                        json!({"reason": "red_window", "window": name, "remaining_pct": remaining}),
                    ),
                    probe,
                ));
            }
        }
        if known {
            probe = json!({"limits": "known"});
        }
    }
    if let Some(conversation_id) = existing_conversation {
        // Замок уже взят выше: повторное чтение видит то же состояние.
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
    } else if !provider.is_empty() {
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
) -> Result<(), AdmitEnd> {
    let attempt_id = Uuid::now_v7();
    let mut conn = scheduler
        .pool()
        .acquire()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    // Условный переход (A): чужую строку не трогаем.
    let mut detail = run.detail.clone();
    merge(
        &mut detail,
        json!({"precheck_attempt_id": attempt_id, "precheck_started_at": Utc::now()}),
    );
    let moved = super::transition_run(
        &mut conn,
        run.id,
        RunState::Claimed,
        run.attempt_id,
        RunState::Prechecking,
        detail,
        None,
        None,
        None,
        None,
    )
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    if !moved {
        return Err(AdmitEnd::Superseded);
    }
    drop(conn);
    let fail = |skip: Skip| stop(skip, RunState::Prechecking);
    let (workdir, timeout_seconds) = match &schedule.target {
        ScheduleTarget::NewSession { workdir, .. } => {
            (workdir.clone(), schedule.precheck_timeout_seconds)
        }
        ScheduleTarget::ExistingSession { .. } => {
            let Some(workdir) = existing_workdir else {
                return Err(fail(Skip::new(RunState::Failed, "conversation_not_found")));
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
            return Err(fail(Skip::with(
                RunState::Failed,
                json!({"reason": "precheck_capability_missing", "capability": CAPABILITY_SCHEDULE_PRECHECK_V1}),
            )));
        }
        Err(PrecheckFailure::NoSession | PrecheckFailure::NoAnswer | PrecheckFailure::Busy) => {
            return Err(fail(Skip::new(RunState::Failed, "precheck_result_unknown")));
        }
        Err(PrecheckFailure::Invalid) => {
            return Err(fail(Skip::new(RunState::Failed, "precheck_invalid_attempt")));
        }
    };
    if reply.attempt_id != attempt_id.to_string() {
        return Err(fail(Skip::new(
            RunState::Failed,
            "precheck_attempt_mismatch",
        )));
    }
    match reply.outcome {
        Some(canterlot_proto::worker::v1::precheck_result::Outcome::Finished(finished)) => {
            if finished.exit_code == 0 && !finished.timed_out {
                Ok(())
            } else {
                Err(fail(Skip::with(
                    RunState::SkippedPrecheck,
                    json!({
                        "reason": if finished.timed_out { "precheck_timeout" } else { "precheck_nonzero" },
                        "exit_code": finished.exit_code,
                        "output_tail": finished.output_tail,
                    }),
                )))
            }
        }
        Some(canterlot_proto::worker::v1::precheck_result::Outcome::Error(error)) => {
            Err(fail(Skip::with(
                RunState::Failed,
                json!({"reason": "precheck_error", "detail": error.reason}),
            )))
        }
        None => Err(fail(Skip::new(RunState::Failed, "precheck_empty_reply"))),
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
    from: RunState,
) -> AdmitResult {
    let fail = |skip: Skip| stop(skip, from);
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
        return Err(fail(Skip::new(RunState::SkippedChanged, "schedule_deleted")));
    };
    // Владение строкой (A): чужая приёмка или восстановление уже забрали
    // запуск — второй сессии не создаём, ничего не перезаписываем.
    let owned: Option<(String, Option<Uuid>)> =
        sqlx::query_as("select state,attempt_id from schedule_runs where id=$1")
            .bind(run.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    let Some((state, attempt)) = owned else {
        let _ = tx.rollback().await;
        return Err(fail(Skip::new(RunState::SkippedChanged, "schedule_deleted")));
    };
    if state != from.as_str() || attempt != run.attempt_id {
        let _ = tx.rollback().await;
        return Err(AdmitEnd::Superseded);
    }
    // Повтор после сбоя сразу после commit dispatch: не создаём вторую сессию
    // и не ставим второй ввод; возвращаем уже зафиксированные идентификаторы.
    // (Состояние `started` при нашем владении сюда попасть не может, но ветвь
    // оставляем как страховку от повторного вызова после commit.)
    let already: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select session_id,input_id from schedule_runs where id=$1 and state='started'",
    )
    .bind(run.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))?;
    if already.is_some() {
        let _ = tx.rollback().await;
        return Ok(AdmitOk);
    }
    let current = current
        .into_schedule()
        .map_err(|_| Skip::new(RunState::Failed, "invalid_schedule"))
        .map_err(fail)?;
    // Ручной запуск с кнопки разрешён и на приостановленном расписании:
    // важно лишь, что его не правили посреди приёма (ревизия та же).
    // Пауза во время приёма тихого тика — это `skipped_changed`.
    if current.revision != run.claimed_revision {
        let _ = tx.rollback().await;
        return Err(fail(Skip::new(
            RunState::SkippedChanged,
            "schedule_revision_changed",
        )));
    }
    if !manual && !run_anyway && !current.enabled {
        let _ = tx.rollback().await;
        return Err(fail(Skip::new(RunState::SkippedChanged, "schedule_paused")));
    }
    // Замок владельца как у прочих запускаемых сессий: параллельные приёмки
    // одного владельца упорядочиваются здесь, а не расходятся в две сессии.
    crate::conversations::lock_owner(&mut tx, schedule.owner_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
        .map_err(fail)?;
    // Решение о пересечении повторяется под замками dispatch: ранняя проверка
    // в приёме могла пропустить заявку, чей ввод встал в очередь после неё.
    if !run_anyway
        && !manual
        && current.overlap == OverlapPolicy::Skip
        && earlier_blocking(&mut tx, schedule.id, run.id)
            .await
            .map_err(fail)?
            > 0
    {
        let _ = tx.rollback().await;
        return Err(fail(Skip::new(RunState::SkippedOverlap, "earlier_run_in_flight")));
    }
    // Пересчёт параллельности с корнями: `live_counts` в маршрутизации видит
    // только детей (`parent_conversation_id is not null`), а запланированные
    // сессии — корни, как и интерактивные. Два расписания под `max_parallel=1`
    // иначе проходят проверку одновременно.
    if matches!(&schedule.target, ScheduleTarget::NewSession { .. }) {
        let (policy, _) = routing::load_policy(&mut tx, schedule.owner_id)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "routing_policy_unavailable"))
            .map_err(fail)?;
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
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
            .map_err(fail)?;
            if busy >= *limit {
                let _ = tx.rollback().await;
                return Err(fail(Skip::with(
                    RunState::SkippedLimits,
                    json!({"reason": "parallel_busy", "pair": pair, "busy": busy, "limit": limit}),
                )));
            }
        }
    }
    // Действующая модель существующей сессии перечитывается здесь (8):
    // до предпроверки она могла смениться, окно лимитов — вместе с ней.
    let mut executor = executor.clone();
    if let ScheduleTarget::ExistingSession { conversation_id } = &schedule.target {
        let fresh: Option<String> = sqlx::query_scalar(
            "select coalesce(d.effective_model,'') from conversations c
              left join conversation_derived d on d.conversation_id = c.id
              where c.id = $1",
        )
        .bind(conversation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
        .map_err(fail)?;
        if let Some(model) = fresh
            && !model.is_empty()
        {
            executor.model = model;
        }
    }
    // Повторная проверка лимитов внутри транзакции dispatch под замками:
    // между ранней проверкой и commit лежит ожидание воркера,
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
        .await
        .map_err(fail)?;
        limits_probe = probe;
        if let Some(detail) = skip {
            let _ = tx.rollback().await;
            return Err(fail(Skip::with(RunState::SkippedLimits, detail)));
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
            })
            .map_err(fail)?
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
            })
            .map_err(fail)?;
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
            })
            .map_err(fail)?;
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
    // `started` и его событие — одна транзакция dispatch (14): крах между
    // commit'ами больше невозможен. Переход условный (A): чужую строку
    // не перезаписываем.
    let moved = super::transition_run(
        &mut tx,
        run.id,
        from,
        run.attempt_id,
        RunState::Started,
        final_detail.clone(),
        Some(now),
        Some(delay),
        Some(session_id),
        Some(input_id),
    )
    .await
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
    .map_err(fail)?;
    if !moved {
        let _ = tx.rollback().await;
        return Err(AdmitEnd::Superseded);
    }
    let started = apply_outcome(
        run,
        RunState::Started,
        final_detail.clone(),
        Some(session_id),
        Some(input_id),
        Some(now),
        Some(delay),
    );
    let payload = serde_json::to_value(&started)
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
        .map_err(fail)?;
    events::append(
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
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
    .map_err(fail)?;
    tx.commit()
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
        .map_err(fail)?;
    scheduler.dispatcher().trigger(session_id);
    Ok(AdmitOk)
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
        if !matches!(current.state, RunState::Claimed | RunState::Prechecking) {
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
/// Резерв строки запуска атомарен (владелец-широкий уникальный индекс):
/// параллельные запросы с тем же ключом и повтор после обрыва получают
/// тот же запуск вместо второго dispatch; второй ключ на другое расписание
/// или другой `run_anyway` — конфликт, а не второй dispatch (E).
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
    // затем поиск/создание резерва. Владелец-широкий ключ резервируется
    // вместе со строкой запуска, до приёма (E). Долгой работы с воркером внутри нет.
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
            run_anyway,
            json!({"manual": true, "run_anyway": run_anyway}),
        )
        .await?;
        match reserved {
            Some(run) => {
                tx.commit().await?;
                Some((schedule, run))
            }
            None => {
                // Чужой резерв с тем же владелец-широким ключом: тот же
                // запуск — дожидаемся победителя, чужой — конфликт.
                let winner = super::get_run_by_owner_key(&mut tx, owner_id, key).await?;
                tx.rollback().await?;
                let Some(winner) = winner else {
                    return Err(ScheduleError::NotFound);
                };
                if winner.schedule_id == schedule_id && winner.run_anyway == run_anyway {
                    return await_winner_run(&state.pool, owner_id, winner.id).await;
                }
                return Err(ScheduleError::Conflict("idempotency_conflict"));
            }
        }
    };
    let Some((schedule, run)) = reserved else {
        return Err(ScheduleError::NotFound);
    };
    let scheduler = state.scheduler.clone();
    let _guard = super::AdmittingGuard::insert(run.id);
    let result = admit_inner(&scheduler, &schedule, &run, run.run_anyway, run.manual).await;
    match result {
        // Dispatch уже закоммитил `started` и событие: фиксируем только
        // идемпотентный ответ.
        Ok(_) => {
            let mut conn = state.pool.acquire().await?;
            let Some(current) = super::get_run(&mut conn, owner_id, run.id).await? else {
                return Err(ScheduleError::NotFound);
            };
            drop(conn);
            let mut tx = crate::db::begin(&state.pool).await?;
            let response = serde_json::to_value(&current)
                .map_err(|e| ScheduleError::Db(sqlx::Error::Encode(Box::new(e))))?;
            let recorded =
                crate::idempotency::record(&mut tx, owner_id, key, "run_now", &hash, &response, None)
                    .await?;
            if !recorded {
                // Гонку резерва мы уже исключили владелец-широким индексом;
                // чужая запись — только завершённая параллельная операция.
                let _ = tx.rollback().await;
                let mut conn = state.pool.acquire().await?;
                match crate::idempotency::lookup(&mut conn, owner_id, key, "run_now", &hash).await? {
                    crate::idempotency::Begin::Replay(value) => {
                        return serde_json::from_value(value)
                            .map_err(|e| ScheduleError::Db(sqlx::Error::Decode(Box::new(e))));
                    }
                    _ => return Err(ScheduleError::Conflict("idempotency_conflict")),
                }
            }
            tx.commit().await?;
            Ok(current)
        }
        Err(AdmitEnd::Superseded) => {
            // Строку забрало восстановление: исход пишет тихий путь,
            // дожидаемся его вместо второго dispatch.
            let mut conn = state.pool.acquire().await?;
            let winner = super::get_run_by_owner_key(&mut conn, owner_id, key).await?;
            let Some(winner) = winner else {
                return Err(ScheduleError::NotFound);
            };
            await_winner_run(&state.pool, owner_id, winner.id).await
        }
        Err(AdmitEnd::Stop(stop)) => {
            let mut detail = run.detail.clone();
            merge(&mut detail, stop.skip.detail);
            let mut tx = crate::db::begin(&state.pool).await?;
            let schedule_exists: bool = sqlx::query_scalar(
                "select exists(select 1 from schedules where id=$1 for update)",
            )
            .bind(schedule.id)
            .fetch_one(&mut *tx)
            .await?;
            if !schedule_exists {
                let _ = tx.rollback().await;
                return Err(ScheduleError::NotFound);
            }
            let moved = super::transition_run(
                &mut tx,
                run.id,
                stop.from,
                run.attempt_id,
                stop.skip.state,
                detail.clone(),
                None,
                None,
                None,
                None,
            )
            .await?;
            if !moved {
                let _ = tx.rollback().await;
                let mut conn = state.pool.acquire().await?;
                let winner = super::get_run_by_owner_key(&mut conn, owner_id, key).await?;
                let Some(winner) = winner else {
                    return Err(ScheduleError::NotFound);
                };
                return await_winner_run(&state.pool, owner_id, winner.id).await;
            }
            let updated = apply_outcome(&run, stop.skip.state, detail, None, None, None, None);
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
            crate::idempotency::record(&mut tx, owner_id, key, "run_now", &hash, &response, None)
                .await?;
            if stop.skip.state == RunState::Failed {
                let reason = updated
                    .detail
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                crate::notify::enqueue_schedule_failure_in_tx(
                    &mut tx,
                    schedule_id,
                    &schedule.name,
                    reason,
                )
                .await?;
            }
            tx.commit().await?;
            Ok(updated)
        }
    }
}
