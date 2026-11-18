//! Приём заявленного запуска: проверки, предпроверка и dispatch вне тика.

use chrono::{DateTime, Utc};
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
    /// Имитация гибели процесса (только точки отказа тестов): ничего не пишем.
    Crashed,
}
type AdmitResult = Result<AdmitOk, AdmitEnd>;

fn stop(skip: Skip, from: RunState) -> AdmitEnd {
    AdmitEnd::Stop(AdmitStop { skip, from })
}

/// Обработка одного `claimed`-запуска. Все проверки идут вне транзакции тика.
/// Владение строкой — попытка из заявки (A): каждый переход условный,
/// проигрыш — молчаливый выход, исход пишет победитель.
pub async fn admit(scheduler: &Scheduler, claimed: ClaimedRun) {
    // Точка управляемой остановки тестов: заявка есть, приёмка ещё не начата.
    super::fault::stall(claimed.schedule.id).await;
    let _guard = super::AdmittingGuard::insert(claimed.run.id);
    let run = claimed.run;
    let schedule = claimed.schedule;
    let result = admit_inner(scheduler, &schedule, &run, run.run_anyway, run.manual).await;
    let finish = match result {
        // Dispatch закоммитил `started` и событие сам: финалу делать нечего.
        Ok(_) => return,
        Err(AdmitEnd::Superseded | AdmitEnd::Crashed) => return,
        Err(AdmitEnd::Stop(stop)) => stop,
    };
    let mut detail = run.detail.clone();
    merge(&mut detail, finish.skip.detail);
    if let Err(error) = write_outcome_retrying(
        scheduler.pool(),
        &schedule,
        &run,
        finish.from,
        finish.skip.state,
        &detail,
        Uuid::now_v7(),
    )
    .await
    {
        tracing::error!(kind = "schedule_admit_db", error = %crate::dberr::describe(&error), "cannot record run outcome");
    }
}

/// Сбой, после которого итог стоит записать ещё раз: временный сбой базы
/// (взаимоблокировка, сериализация), таймаут замка (`55P03`, обычный исход
/// кратковременной блокировки расписания) и ошибки пула соединений.
fn recoverable(error: &sqlx::Error) -> bool {
    crate::db::TransientError::is_transient(error)
        || matches!(
            error,
            sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03")
        )
        || matches!(error, sqlx::Error::PoolTimedOut | sqlx::Error::Io(_))
}

/// Временный сбой базы не должен оставлять запуск до истечения аренды (5):
/// финал повторяется ограниченное число раз с короткой паузой.
#[doc(hidden)]
pub async fn write_outcome_retrying(
    pool: &PgPool,
    schedule: &Schedule,
    run: &ScheduleRun,
    from: RunState,
    state: RunState,
    detail: &Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    const ATTEMPTS: u32 = 4;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match write_outcome(pool, schedule, run, from, state, detail, correlation_id).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt < ATTEMPTS && recoverable(&error) => {
                tokio::time::sleep(crate::db::retry_delay(Uuid::now_v7(), attempt)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Итог приёма одной транзакцией: под замком расписания (порядок B:
/// расписание, затем запуск), переход условный (A): чужую строку не
/// перезаписываем. `Ok` и при проигрыше владения — исход тогда чужой.
async fn write_outcome(
    pool: &PgPool,
    schedule: &Schedule,
    run: &ScheduleRun,
    from: RunState,
    state: RunState,
    detail: &Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    let schedule_exists: bool =
        sqlx::query_scalar("select exists(select 1 from schedules where id=$1 for update)")
            .bind(schedule.id)
            .fetch_one(&mut *tx)
            .await?;
    if !schedule_exists {
        // Расписание удалили, пока шёл приём: строка запуска ушла
        // каскадом, событие о призраке не публикуем.
        tx.rollback().await?;
        return Ok(());
    }
    let moved = super::transition_run(
        &mut tx,
        run.id,
        from,
        run.attempt_id,
        state,
        detail.clone(),
        None,
        None,
        None,
        None,
    )
    .await?;
    if !moved {
        // Строку забрало восстановление или вторая приёмка: исход чужой.
        tx.rollback().await?;
        return Ok(());
    }
    let updated = apply_outcome(run, state, detail.clone(), None, None, None, None);
    let payload = serde_json::to_value(&updated).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
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
            correlation_id,
        },
    )
    .await?;
    super::finalize_manual_key(&mut tx, schedule.owner_id, &updated).await?;
    if state == RunState::Failed {
        let reason = detail
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        crate::notify::enqueue_schedule_failure_in_tx(&mut tx, schedule.id, &schedule.name, reason)
            .await?;
    }
    tx.commit().await
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
        // Модель — из авторитетной строки: проекция `conversation_derived`
        // отстаёт от подтверждённой смены настроек.
        let row: (String, Option<String>) =
            sqlx::query_as("select c.harness, c.model from conversations c where c.id = $1")
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
        resolve_route(scheduler, &mut conn, schedule, run, run_anyway, &[]).await?
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
    busy_after_routing: &[(String, String)],
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
    let mut live = routing::live_counts_with_roots(conn, schedule.owner_id)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "live_counts_unavailable"))?;
    // Пара, ставшая занятой после маршрутизации (dispatch под замками), не
    // выбирается снова: следующий кандидат вместо отказа.
    for pair in busy_after_routing {
        live.insert(pair.clone(), i64::MAX / 2);
    }
    // Правила расписаний общие с приёмом: тот же порог остатка и свежесть.
    let settings = limit_settings::effective(conn, schedule.owner_id, scheduler.limits_config())
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limit_settings_unavailable"))?;
    let rules = routing::ScheduleRules {
        danger_remaining_pct: f64::from(settings.danger_remaining_pct),
        fresh_for: scheduler.limits_config().stale_after,
    };
    let now = Utc::now();
    // Корневые умолчания нового сеанса (10): пропущенный режим — режим
    // создания (`bypass`), а не дочерний (`read_only`). Иначе маршрутизация
    // отбрасывает харнесс, у которого нет дочернего режима, хотя creation
    // потом всё равно поставил бы `bypass`.
    let mode = explicit
        .clone()
        .unwrap_or_else(|| crate::conversations::DEFAULT_PERMISSION_MODE.to_owned());
    let request = RouteRequest {
        task_type,
        level,
        parent_harness: String::new(),
        parent_mode: String::new(),
        explicit_mode: Some(mode.clone()),
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
        schedule_rules: Some(rules),
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
    // Пресет расписания важнее политики: владелец выбрал его в редакторе.
    let preset = match &schedule.target {
        ScheduleTarget::NewSession {
            preset: schedule_preset,
            ..
        } => schedule_preset
            .clone()
            .unwrap_or_else(|| "clean".to_owned()),
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
    // Замки раньше чтений (8): сначала разделяемая строка провайдера, затем
    // очередь существующей сессии — как у обычной отправки. В транзакции
    // dispatch замки держатся до commit, поэтому окно не краснеет и
    // удержание не встаёт между чтением и замком.
    if !provider.is_empty() {
        limits::share_provider(conn, provider)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
    }
    if let Some(conversation_id) = existing_conversation {
        sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "queue_unavailable"))?;
    }
    let settings = limit_settings::effective(conn, owner_id, config)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limit_settings_unavailable"))?;
    let all = limits::current(conn)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))?;
    // Предикат окон один с маршрутизацией (`schedule_window_verdict`):
    // строгое сравнение остатка, как PWA, только свежие показания.
    let mut probe = json!({"limits": "unknown"});
    if let Some(entry) = all.iter().find(|entry| entry.provider == provider) {
        // Окно Fable — только для Claude-моделей Fable, как в маршрутизации.
        let fable = provider == "claude" && model.to_ascii_lowercase().contains("fable");
        let reading = |window: Option<&limits::WindowState>| {
            window.map(|w| {
                (
                    f64::from(w.used_permille) / 10.0,
                    w.observed_at,
                    w.resets_at,
                )
            })
        };
        let mut windows = vec![
            ("five_hour", reading(entry.five_hour.as_ref())),
            ("weekly", reading(entry.weekly.as_ref())),
        ];
        if fable {
            windows.push(("weekly_fable", reading(entry.weekly_fable.as_ref())));
        }
        let verdict = routing::schedule_window_verdict(
            windows,
            routing::ScheduleRules {
                danger_remaining_pct: f64::from(settings.danger_remaining_pct),
                fresh_for: config.stale_after,
            },
            now,
        );
        if let Some((name, remaining)) = verdict.red {
            return Ok((
                Some(json!({"reason": "red_window", "window": name, "remaining_pct": remaining})),
                probe,
            ));
        }
        if verdict.known {
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
            return Err(fail(Skip::new(
                RunState::Failed,
                "precheck_invalid_attempt",
            )));
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
    let begun = if super::fault::hit(schedule.id, super::fault::Fault::DispatchBeginError) {
        Err(sqlx::Error::PoolTimedOut)
    } else {
        crate::db::begin(scheduler.pool()).await
    };
    let mut tx = begun
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
        .map_err(fail)?;
    let current: Option<super::ScheduleRow> =
        sqlx::query_as("select * from schedules where id=$1 and owner_id=$2 for update")
            .bind(schedule.id)
            .bind(schedule.owner_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
            .map_err(fail)?;
    let Some(current) = current else {
        let _ = tx.rollback().await;
        return Err(fail(Skip::new(
            RunState::SkippedChanged,
            "schedule_deleted",
        )));
    };
    // Владение строкой (A): чужая приёмка или восстановление уже забрали
    // запуск — второй сессии не создаём, ничего не перезаписываем.
    let owned: Option<(String, Option<Uuid>)> =
        sqlx::query_as("select state,attempt_id from schedule_runs where id=$1")
            .bind(run.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
            .map_err(fail)?;
    let Some((state, attempt)) = owned else {
        let _ = tx.rollback().await;
        return Err(fail(Skip::new(
            RunState::SkippedChanged,
            "schedule_deleted",
        )));
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
    .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
    .map_err(fail)?;
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
    // Порядок замков как у обычного создания: владелец, машина, затем строка
    // провайдера (лимиты). Иначе создание держит провайдера и ждёт машину, пока
    // отзыв держит машину и ждёт учётные данные, а приём журнала держит
    // учётные данные и ждёт провайдера: цикл из трёх транзакций.
    if let ScheduleTarget::NewSession { machine_id, .. } = &schedule.target {
        sqlx::query("select id from machines where id=$1 and owner_id=$2 for no key update")
            .bind(machine_id)
            .bind(schedule.owner_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
            .map_err(fail)?;
    }
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
        return Err(fail(Skip::new(
            RunState::SkippedOverlap,
            "earlier_run_in_flight",
        )));
    }
    let mut executor = executor.clone();
    let mut provider = provider.to_owned();
    // Пересчёт параллельности с корнями: `live_counts` в маршрутизации видит
    // только детей (`parent_conversation_id is not null`), а запланированные
    // сессии — корни, как и интерактивные. Два расписания под `max_parallel=1`
    // иначе проходят проверку одновременно. Занятая после маршрутизации пара
    // не отказывает сразу (F): маршрутизируемая цель выбирает следующего
    // кандидата под теми же замками.
    if matches!(&schedule.target, ScheduleTarget::NewSession { .. }) {
        let routed = matches!(
            &schedule.target,
            ScheduleTarget::NewSession {
                executor: None,
                route: Some(_),
                ..
            }
        );
        let mut busy_pairs: Vec<(String, String)> = Vec::new();
        loop {
            let (policy, _) = routing::load_policy(&mut tx, schedule.owner_id)
                .await
                .map_err(|_| Skip::new(RunState::Failed, "routing_policy_unavailable"))
                .map_err(fail)?;
            let pair = format!("{}/{}", executor.harness, executor.model);
            let Some(limit) = policy.max_parallel.get(&pair) else {
                break;
            };
            // Тот же запрос занятости, что в маршрутизации (с корнями и
            // ожидающими толчками): иначе явный исполнитель и маршрут расходятся.
            let busy: i64 = routing::live_counts_with_roots(&mut tx, schedule.owner_id)
                .await
                .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
                .map_err(fail)?
                .get(&(executor.harness.clone(), executor.model.clone()))
                .copied()
                .unwrap_or(0);
            let injected = super::fault::hit(schedule.id, super::fault::Fault::BusyAfterRouting);
            if busy < *limit && !injected {
                break;
            }
            let busy_detail =
                json!({"reason": "parallel_busy", "pair": pair, "busy": busy, "limit": limit});
            if !routed {
                let _ = tx.rollback().await;
                return Err(fail(Skip::with(RunState::SkippedLimits, busy_detail)));
            }
            busy_pairs.push((executor.harness.clone(), executor.model.clone()));
            match resolve_route(scheduler, &mut tx, schedule, run, run_anyway, &busy_pairs).await {
                Ok(next) => {
                    provider = canterlot_proto::harness::provider_of(&next.harness)
                        .unwrap_or("")
                        .to_owned();
                    executor = next;
                }
                Err(skip) => {
                    let _ = tx.rollback().await;
                    return Err(fail(skip));
                }
            }
        }
    }
    // Действующая модель существующей сессии перечитывается под замками (8):
    // сначала разделяемая строка провайдера, затем очередь, и только потом
    // чтение. До предпроверки модель могла смениться, окно — вместе с ней.
    if let ScheduleTarget::ExistingSession { conversation_id } = &schedule.target {
        if !provider.is_empty() {
            limits::share_provider(&mut tx, &provider)
                .await
                .map_err(|_| Skip::new(RunState::Failed, "limits_unavailable"))
                .map_err(fail)?;
        }
        sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
            .bind(conversation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Skip::new(RunState::Failed, "queue_unavailable"))
            .map_err(fail)?;
        // Авторитетная модель под замком очереди: её меняет транзакция
        // подтверждённых настроек, проекция может отставать.
        // Ожидающая смена модели уходит с вводом при выдаче (delivery::dispatch),
        // поэтому окна проверяются по ней, если она есть: решение и выдача
        // читают одну и ту же строку под одним замком очереди и не расходятся.
        let fresh: Option<String> = sqlx::query_scalar(
            "select coalesce(nullif(c.pending_settings->>'model', ''), c.model)
                   from conversations c where c.id = $1",
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
            &provider,
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
    // Ответ ручного запуска — в той же транзакции (3): после commit
    // повтор по ключу получает уже начатый запуск, а не второй dispatch.
    super::finalize_manual_key(&mut tx, schedule.owner_id, &started)
        .await
        .map_err(|_| Skip::new(RunState::Failed, "database_unavailable"))
        .map_err(fail)?;
    if super::fault::hit(schedule.id, super::fault::Fault::BeforeDispatchCommit) {
        drop(tx);
        return Err(AdmitEnd::Crashed);
    }
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

/// Ждёт исхода запуска, не держа соединение между опросами. Если строка
/// истории исчезла (уборка успела после финала), итог берётся из долговечной
/// записи общего ключа: она пишется в той же транзакции, что и итог.
async fn await_winner_run(
    pool: &PgPool,
    owner_id: Uuid,
    run_id: Uuid,
    key: &str,
) -> Result<ScheduleRun, ScheduleError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let mut conn = pool.acquire().await?;
        let Some(current) = super::get_run(&mut conn, owner_id, run_id).await? else {
            return durable_run(&mut conn, owner_id, run_id, key)
                .await?
                .ok_or(ScheduleError::NotFound);
        };
        drop(conn);
        if !matches!(current.state, RunState::Claimed | RunState::Prechecking)
            || std::time::Instant::now() >= deadline
        {
            return Ok(current);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Ответ Run Now из общего ключа идемпотентности, если он про этот запуск.
async fn durable_run(
    conn: &mut PgConnection,
    owner_id: Uuid,
    run_id: Uuid,
    key: &str,
) -> Result<Option<ScheduleRun>, ScheduleError> {
    let stored: Option<serde_json::Value> = sqlx::query_scalar(
        "select response from idempotency_keys
          where owner_id=$1 and key=$2 and operation='run_now' and response->>'id'=$3",
    )
    .bind(owner_id)
    .bind(key)
    .bind(run_id.to_string())
    .fetch_optional(conn)
    .await?;
    stored
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|e| ScheduleError::Db(sqlx::Error::Decode(Box::new(e))))
        })
        .transpose()
}

/// Итог резерва ручного запуска.
pub enum Reservation {
    /// Строка запуска и ключ заняты этим вызовом: приём за ним.
    Fresh {
        schedule: Box<Schedule>,
        run: ScheduleRun,
    },
    /// Ключ уже занят тем же запросом: запуск был заявлен раньше.
    Existing(ScheduleRun),
}

/// Резерв ручного запуска (3): общий ключ идемпотентности и строка запуска
/// занимаются одной транзакцией до приёма. Ключ живёт срок идемпотентности
/// API независимо от журнала запусков, а ответ дописывает итоговая транзакция
/// приёма или восстановления. Чужая операция с тем же ключом получает конфликт
/// сразу, а не после начатой сессии.
pub async fn reserve_manual(
    pool: &PgPool,
    owner_id: Uuid,
    schedule_id: Uuid,
    key: &str,
    run_anyway: bool,
) -> Result<Reservation, ScheduleError> {
    // Проигранная гонка за общий ключ откатывает заявку целиком; победителя
    // разбирает следующий проход по ключу (повтор или конфликт).
    for _ in 0..3 {
        if let Some(reservation) =
            reserve_manual_once(pool, owner_id, schedule_id, key, run_anyway).await?
        {
            return Ok(reservation);
        }
    }
    Err(ScheduleError::Conflict("idempotency_conflict"))
}

async fn reserve_manual_once(
    pool: &PgPool,
    owner_id: Uuid,
    schedule_id: Uuid,
    key: &str,
    run_anyway: bool,
) -> Result<Option<Reservation>, ScheduleError> {
    let body = json!({"schedule_id": schedule_id, "run_anyway": run_anyway});
    let hash = crate::idempotency::request_hash("run_now", &body);
    let mut tx = crate::db::begin(pool).await?;
    lock_idempotency_key(&mut tx, owner_id, key).await?;
    match crate::idempotency::lookup(&mut tx, owner_id, key, "run_now", &hash).await? {
        crate::idempotency::Begin::Replay(value) => {
            let _ = tx.rollback().await;
            let run = serde_json::from_value(value)
                .map_err(|e| ScheduleError::Db(sqlx::Error::Decode(Box::new(e))))?;
            return Ok(Some(Reservation::Existing(run)));
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
    let reserved = super::reserve_run(
        &mut tx,
        Uuid::now_v7(),
        schedule_id,
        owner_id,
        Utc::now(),
        schedule.revision,
        key,
        run_anyway,
        json!({"manual": true, "run_anyway": run_anyway}),
    )
    .await?;
    let Some(run) = reserved else {
        // Запись общего ключа уже убрана по сроку, а ручная строка истории
        // осталась: тот же запрос (расписание и `run_anyway`) получает её
        // обратно, чужой запрос под этим ключом — конфликт.
        let retained: Option<super::RunRow> = sqlx::query_as(
            "select * from schedule_runs where owner_id=$1 and idempotency_key=$2 and manual",
        )
        .bind(owner_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let _ = tx.rollback().await;
        let retained = retained.map(super::RunRow::into_run).transpose()?;
        return match retained {
            Some(run) if run.schedule_id == schedule_id && run.run_anyway == run_anyway => {
                Ok(Some(Reservation::Existing(run)))
            }
            _ => Err(ScheduleError::Conflict("idempotency_conflict")),
        };
    };
    let response = serde_json::to_value(&run)
        .map_err(|e| ScheduleError::Db(sqlx::Error::Encode(Box::new(e))))?;
    // Общий ключ арбитрирует гонку уникальным вставлением (композер делает так
    // же, не беря advisory-замок): без успешной вставки заявку коммитить нельзя.
    if !crate::idempotency::record(&mut tx, owner_id, key, "run_now", &hash, &response, None)
        .await?
    {
        let _ = tx.rollback().await;
        return Ok(None);
    }
    tx.commit().await?;
    Ok(Some(Reservation::Fresh {
        schedule: Box::new(schedule),
        run,
    }))
}

/// Повтор уже заявленного запуска: дожидаемся исхода или отдаём итог.
pub async fn settle_existing(
    pool: &PgPool,
    owner_id: Uuid,
    stored: ScheduleRun,
) -> Result<ScheduleRun, ScheduleError> {
    if matches!(stored.state, RunState::Claimed | RunState::Prechecking) {
        await_winner_run(pool, owner_id, stored.id, &stored.idempotency_key).await
    } else {
        Ok(stored)
    }
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
    let (schedule, run) =
        match reserve_manual(&state.pool, owner_id, schedule_id, key, run_anyway).await? {
            Reservation::Fresh { schedule, run } => (*schedule, run),
            Reservation::Existing(stored) => {
                return settle_existing(&state.pool, owner_id, stored).await;
            }
        };
    let scheduler = state.scheduler.clone();
    let _guard = super::AdmittingGuard::insert(run.id);
    let result = admit_inner(&scheduler, &schedule, &run, run.run_anyway, run.manual).await;
    match result {
        // Dispatch закоммитил `started`, событие и ответ ключа сам.
        Ok(_) => {
            if super::fault::hit(schedule_id, super::fault::Fault::LostRunNowResponse) {
                return Err(ScheduleError::Db(sqlx::Error::PoolClosed));
            }
            let mut conn = state.pool.acquire().await?;
            super::get_run(&mut conn, owner_id, run.id)
                .await?
                .ok_or(ScheduleError::NotFound)
        }
        Err(AdmitEnd::Crashed) => Err(ScheduleError::Db(sqlx::Error::PoolClosed)),
        // Строку забрало восстановление: исход пишет тихий путь,
        // дожидаемся его вместо второго dispatch.
        Err(AdmitEnd::Superseded) => {
            await_winner_run(&state.pool, owner_id, run.id, &run.idempotency_key).await
        }
        Err(AdmitEnd::Stop(stop)) => {
            let mut detail = run.detail.clone();
            merge(&mut detail, stop.skip.detail);
            write_outcome_retrying(
                &state.pool,
                &schedule,
                &run,
                stop.from,
                stop.skip.state,
                &detail,
                correlation_id,
            )
            .await?;
            // Исход мог записать и другой владелец строки: отвечаем тем, что в базе.
            await_winner_run(&state.pool, owner_id, run.id, &run.idempotency_key).await
        }
    }
}
