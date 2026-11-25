//! Усыпление простаивающих сессий.
//!
//! Сессию можно усыпить только после доказанного простоя: хаб видит состояние `idle`
//! со свежей телеметрией живого процесса, без фоновых работ и без всех остальных
//! оснований держать CLI запущенным. Усыпление — обычная остановка с причиной
//! `hibernated`, а не сбой; следующее сообщение будит сессию новым исполнением с тем же
//! нативным id. Так же будит и остановка перезагрузкой машины (`machine_rebooted`):
//! остальные причины остановки ждут владельца. Никаких уведомлений владельцу усыпление
//! не посылает.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, types::Json};
use tokio::time::Instant;
use uuid::Uuid;

use crate::{
    delivery::LockedQueue,
    events::{self, NewEvent, Scope},
    executions,
    model::{PauseReason, RunSlot},
    presence::Presence,
};

/// Сколько минут простоя считается достаточным, пока владелец не выбрал своё значение.
pub const DEFAULT_IDLE_MINUTES: i32 = 60;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HibernateConfig {
    #[serde(with = "humantime_serde")]
    pub sweep: Duration,
    /// Сколько после старта процесса хаба обход ничего не усыпляет. Сессия, давно
    /// простаивавшая до выката, иначе засыпает сразу после запуска нового хаба.
    #[serde(with = "humantime_serde")]
    pub startup_grace: Duration,
}

impl Default for HibernateConfig {
    fn default() -> Self {
        Self {
            sweep: Duration::from_secs(60),
            startup_grace: DEFAULT_STARTUP_GRACE,
        }
    }
}

/// Окно после старта хаба, в которое усыпление не работает, пока владелец не выбрал своё.
pub const DEFAULT_STARTUP_GRACE: Duration = Duration::from_secs(600);

/// Окно тишины после старта процесса: считается по монотонным часам, чтобы тест двигал
/// момент арифметикой, а не ожиданием.
#[derive(Debug, Clone, Copy)]
pub struct StartupGrace {
    until: Instant,
}

impl StartupGrace {
    pub fn new(started: Instant, window: Duration) -> Self {
        Self {
            until: started + window,
        }
    }

    pub fn is_over(&self, now: Instant) -> bool {
        now >= self.until
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HibernateSettings {
    /// `None` — «Выключено».
    #[serde(default)]
    pub idle_minutes: Option<i32>,
}

impl Default for HibernateSettings {
    fn default() -> Self {
        Self {
            idle_minutes: Some(DEFAULT_IDLE_MINUTES),
        }
    }
}

pub async fn settings(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<HibernateSettings, sqlx::Error> {
    let stored: Option<Option<i32>> =
        sqlx::query_scalar("select idle_minutes from hibernate_settings where owner_id=$1")
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(HibernateSettings {
        idle_minutes: match stored {
            None => Some(DEFAULT_IDLE_MINUTES),
            Some(minutes) => minutes,
        },
    })
}

/// `None` — усыпление выключено для владельца.
pub async fn effective_minutes(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Option<i32>, sqlx::Error> {
    Ok(settings(conn, owner_id).await?.idle_minutes)
}

pub async fn replace_settings_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    value: &HibernateSettings,
) -> Result<(), sqlx::Error> {
    if let Some(minutes) = value.idle_minutes
        && !(1..=1440).contains(&minutes)
    {
        return Err(sqlx::Error::InvalidArgument(
            "idle_minutes must be between 1 and 1440".into(),
        ));
    }
    sqlx::query(
        "insert into hibernate_settings(owner_id,idle_minutes,updated_at)
         values($1,$2,clock_timestamp())
         on conflict(owner_id) do update set idle_minutes=excluded.idle_minutes,
             updated_at=clock_timestamp()",
    )
    .bind(owner_id)
    .bind(value.idle_minutes)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    harness: String,
}

/// Все сессии, которые по данным хаба достаточно долго доказательно простаивают.
/// Проверки, которые живут только в памяти процесса (присутствие в PWA), делает `sweep`.
async fn candidates(pool: &PgPool) -> Result<Vec<Candidate>, sqlx::Error> {
    sqlx::query_as(
        r#"select c.id,c.harness from conversations c
         join conversation_derived d on d.conversation_id=c.id
         left join hibernate_settings hs on hs.owner_id=c.owner_id
         where c.trashed_at is null
           and d.activity_state='idle'
           and d.activity_evidence->>'kind'='idle'
           and coalesce((d.activity_evidence->>'process_alive')::bool,false)=true
           and coalesce((d.activity_evidence->>'background_work')::int,0)=0
           and d.activity_since < now() - (
                 case when hs.owner_id is null then $1 else hs.idle_minutes end
               ) * interval '1 minute'
           and exists(select 1 from executions e where e.conversation_id=c.id
                      and e.state in ('created','starting','running','stopping'))
           and not exists(select 1 from conversations child
                          where child.parent_conversation_id=c.id
                            and exists(select 1 from executions e where e.conversation_id=child.id
                                       and e.state in ('created','starting','running','stopping')))
           and not exists(select 1 from alarms a where a.state='armed'
                          and (a.conversation_id=c.id or a.condition->'session_ids' ? c.id::text))
           and not exists(select 1 from session_subscriptions s where s.closed_at is null
                          and (s.target_id=c.id or s.subscriber_id=c.id))
           and not exists(select 1 from nudge_rules nr where nr.conversation_id=c.id
                          and nr.state in ('watching','pending'))
           and not exists(select 1 from plan_tasks p where p.executor_conversation_id=c.id
                          and p.state='in_progress')
           and not exists(select 1 from inputs i where i.conversation_id=c.id
                          and i.state in ('queued','dispatched','on_machine','transmitting',
                                          'acceptance_unconfirmed'))
           and not exists(select 1 from requests r where r.conversation_id=c.id
                          and r.state='open')
           and not exists(select 1 from secret_requests sr where sr.conversation_id=c.id
                          and sr.state in ('open','lost'))
           and not exists(select 1 from queue_state q where q.conversation_id=c.id
                          and (q.run_slot<>'free'
                               or jsonb_array_length(q.hold_reasons)>0
                               or q.pause_reasons @> '[{"reason":"limit_reached"}]'::jsonb
                               or q.pause_reasons @> '[{"reason":"session_stuck"}]'::jsonb
                               or q.pause_reasons @> '[{"reason":"unconfirmed_delivery"}]'::jsonb))"#,
    )
    .bind(DEFAULT_IDLE_MINUTES)
    .fetch_all(pool)
    .await
}

/// Один обход без окна после старта. Возвращает число усыплённых сессий.
pub async fn sweep(pool: &PgPool, presence: &Presence) -> Result<usize, sqlx::Error> {
    sweep_from(pool, presence, Instant::now()).await
}

/// Обход в момент `now`: пока окно после старта хаба не кончилось, не усыпляет ничего.
pub async fn sweep_at(
    pool: &PgPool,
    presence: &Presence,
    grace: &StartupGrace,
    now: Instant,
) -> Result<usize, sqlx::Error> {
    if !grace.is_over(now) {
        return Ok(0);
    }
    sweep_from(pool, presence, now).await
}

async fn sweep_from(
    pool: &PgPool,
    presence: &Presence,
    now: Instant,
) -> Result<usize, sqlx::Error> {
    let mut count = 0;
    for candidate in candidates(pool).await? {
        if presence.is_visible(candidate.id, now) {
            continue;
        }
        if !canterlot_proto::harness::supports_resume(&candidate.harness) {
            continue;
        }
        let id = candidate.id;
        let changed = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move { hibernate_in_tx(&mut *conn, id).await })
        })
        .await?;
        if changed {
            count += 1;
        }
    }
    Ok(count)
}

/// Усыпляет сессию в чужой транзакции. Повтор ничего не меняет.
pub async fn hibernate_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id=$1 for update")
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
    let owner_id: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    if effective_minutes(&mut *conn, owner_id).await?.is_none() {
        return Ok(false);
    }
    // Все условия проверяются ещё раз под блокировкой очереди: между выборкой и здесь
    // мог прийти ввод или смениться состояние.
    //
    // Проекция активности отстаёт от очереди: ввод успевает дойти до агента раньше, чем
    // проектор пересчитает сессию, и `idle` с давней отметкой ещё стоит. Поэтому идущий
    // ход проверяется по первичным данным — слоту очереди и фазе исполнения.
    //
    // Неопределённый исход передачи (`AcceptanceUndetermined`) освобождает слот, хотя CLI
    // мог ввод получить и работать; до прояснения исхода сессию не усыпляем.
    if queue.slot()? != RunSlot::Free
        || queue
            .pause_reasons
            .0
            .iter()
            .any(|reason| matches!(reason, PauseReason::UnconfirmedDelivery { .. }))
        || executions::turn_in_progress(&mut *conn, conversation_id).await?
    {
        return Ok(false);
    }
    let eligible: bool = sqlx::query_scalar(
        "select d.activity_state='idle'
            and d.activity_evidence->>'kind'='idle'
            and coalesce((d.activity_evidence->>'process_alive')::bool,false)=true
            and coalesce((d.activity_evidence->>'background_work')::int,0)=0
            and not exists(select 1 from inputs i where i.conversation_id=d.conversation_id
                           and i.state in ('queued','dispatched','on_machine','transmitting',
                                           'acceptance_unconfirmed'))
            and not exists(select 1 from requests r where r.conversation_id=d.conversation_id
                           and r.state='open')
            and not exists(select 1 from secret_requests sr where sr.conversation_id=d.conversation_id
                           and sr.state in ('open','lost'))
         from conversation_derived d where d.conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(false);
    if !eligible {
        return Ok(false);
    }
    let Some((execution_id, generation, _)) = executions::live(&mut *conn, conversation_id).await?
    else {
        return Ok(false);
    };
    if executions::resumable_native_id(&mut *conn, conversation_id)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let pending: bool = sqlx::query_scalar(
        "select exists(select 1 from commands where conversation_id=$1
            and kind='stop_execution' and delivery_state<>'completed'
            and payload->>'reason'='hibernated')",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if pending {
        return Ok(false);
    }
    sqlx::query("update executions set hibernate_requested=true where id=$1")
        .bind(execution_id)
        .execute(&mut *conn)
        .await?;
    if !queue.pause_reasons.0.contains(&PauseReason::Hibernated) {
        queue.pause_reasons.0.push(PauseReason::Hibernated);
    }
    let slot = queue.slot()?;
    queue
        .save(&mut *conn, &slot, Some(execution_id), Uuid::now_v7())
        .await?;
    let machine_id: Uuid = sqlx::query_scalar(
        "insert into commands (id,machine_id,conversation_id,execution_id,
                               execution_generation,kind,payload,delivery_state)
         select $2,machine_id,id,$4,$5,'stop_execution',$6,'pending'
         from conversations where id=$1 returning machine_id",
    )
    .bind(conversation_id)
    .bind(Uuid::now_v7())
    .bind(owner_id)
    .bind(execution_id)
    .bind(generation)
    .bind(json!({"force": false, "reason": "hibernated"}))
    .fetch_one(&mut *conn)
    .await?;
    crate::delivery::control::notify_outbox(&mut *conn, machine_id).await?;
    events::append(
        &mut *conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "queue_paused",
            conversation_id: Some(conversation_id),
            execution_id: Some(execution_id),
            payload: json!({"pause_reasons": queue.pause_reasons.0}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    Ok(true)
}

/// Будит усыплённую сессию или сессию, остановленную перезагрузкой машины, если для неё
/// есть отложенный ввод. Вызывается и при приёме сообщения, и при завершении остановки:
/// сообщение, пришедшее во время остановки, дождётся её конца и будет доставлено ровно
/// один раз новым исполнением. Прочие остановки — `user_stop`, `force_stopped` и сбои —
/// ждут владельца и сами не возобновляются. `Ok(false)` — сессию не разбудили: будить
/// нечего либо старт отказал, и ввод остаётся в очереди.
pub async fn wake_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let pauses: Json<Vec<PauseReason>> =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let latest: Option<(String, Option<String>, bool)> = sqlx::query_as(
        "select state, outcome, hibernate_requested from executions
         where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let hibernated = pauses.0.contains(&PauseReason::Hibernated);
    // Остановку перезагрузкой записывает воркер при следующем старте: разговор, как и
    // усыплённый, просыпается следующим вводом, а не сам по себе.
    let rebooted = latest.as_ref().is_some_and(|(state, outcome, _)| {
        state == "stopped" && outcome.as_deref() == Some("machine_rebooted")
    });
    let hibernate_requested = latest.as_ref().is_some_and(|(_, _, requested)| *requested);
    if !hibernated && !hibernate_requested && !rebooted {
        return Ok(false);
    }
    // Остановку владельца не отменяет ни перезагрузка машины, ни пришедший ввод: пауза
    // `user_stop` ставится раньше команды остановки и снимается только владельцем.
    // Иначе машина, перезагрузившаяся до применения остановки, подняла бы процесс снова.
    if pauses.0.contains(&PauseReason::UserStop) {
        return Ok(false);
    }
    if executions::live(&mut *conn, conversation_id)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1
         and state in ('queued','dispatched','on_machine')",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if queued == 0 {
        return Ok(false);
    }
    let owner_id: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    match executions::start_in_tx(&mut *conn, owner_id, conversation_id, correlation_id).await {
        Ok(_) => {}
        Err(executions::ExecutionError::Db(error)) => return Err(error),
        // Поднять сессию можно не всегда (раннер, снятая рабочая копия, закрытый разговор).
        // Это не сбой вызывающего: ввод остаётся в очереди и ждёт владельца, как у любой
        // остановленной сессии. Ошибка здесь откатила бы приём сообщения, а на пути
        // журнала — остановила бы приём записей всей машины. Старт отказывает раньше, чем
        // что-либо меняет в разговоре, откатывать нечего.
        Err(refused) => {
            tracing::warn!(
                conversation_id = %conversation_id,
                reason = %refused,
                "hibernate wake refused; the input stays queued"
            );
            return Ok(false);
        }
    }
    if hibernated || rebooted {
        let mut queue: LockedQueue =
            sqlx::query_as("select * from queue_state where conversation_id=$1 for update")
                .bind(conversation_id)
                .fetch_one(&mut *conn)
                .await?;
        let before = queue.pause_reasons.0.clone();
        if hibernated {
            queue
                .pause_reasons
                .0
                .retain(|reason| *reason != PauseReason::Hibernated);
        }
        // Перезагрузка объясняет и потерю процесса, и потерю владельца: обе паузы снимает
        // тот же ввод, что будит сессию. Остальные причины остановки остаются.
        if rebooted {
            queue.pause_reasons.0.retain(|reason| {
                !matches!(reason, PauseReason::ProcessExited | PauseReason::OwnerLost)
            });
        }
        if queue.pause_reasons.0 != before {
            let slot = queue.slot()?;
            queue.save(&mut *conn, &slot, None, correlation_id).await?;
            events::append(
                &mut *conn,
                NewEvent {
                    scope: Scope::Conversation {
                        id: conversation_id,
                    },
                    kind: "queue_resumed",
                    conversation_id: Some(conversation_id),
                    execution_id: None,
                    payload: json!({"pause_reasons": queue.pause_reasons.0}),
                    correlation_id,
                },
            )
            .await?;
        }
    }
    Ok(true)
}

/// Фоновая служба: обходит кандидатов раз в `sweep`, не чаще одного обхода за interval;
/// первые `startup_grace` после своего старта ничего не усыпляет.
pub async fn run(
    pool: PgPool,
    presence: Presence,
    interval: Duration,
    startup_grace: Duration,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    let grace = StartupGrace::new(Instant::now(), startup_grace);
    let mut ticker = tokio::time::interval(interval.max(Duration::from_millis(10)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(error) = sweep_at(&pool, &presence, &grace, Instant::now()).await {
                    tracing::warn!(error = %crate::dberr::describe(&error), "hibernate sweep failed");
                }
            }
            _ = stopped.changed() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_an_hour_and_off_is_none() {
        assert_eq!(HibernateSettings::default().idle_minutes, Some(60));
        assert_eq!(
            serde_json::from_str::<HibernateSettings>(r#"{"idle_minutes":null}"#)
                .unwrap()
                .idle_minutes,
            None
        );
    }

    #[test]
    fn startup_grace_defaults_to_ten_minutes_and_parses() {
        assert_eq!(
            HibernateConfig::default().startup_grace,
            Duration::from_secs(600)
        );
        let parsed: HibernateConfig = toml::from_str("startup_grace = \"3m\"").unwrap();
        assert_eq!(parsed.startup_grace, Duration::from_secs(180));
        assert_eq!(parsed.sweep, Duration::from_secs(60));
    }
}
